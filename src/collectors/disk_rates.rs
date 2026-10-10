//! Volume counters need their own baseline: sysinfo's first delta can be the lifetime total.
use std::collections::HashMap;

use crate::model::DiskSnapshot;

type Identity = (String, String, String);

#[derive(Default)]
pub(super) struct DiskRates {
    previous: HashMap<Identity, (u64, u64)>,
}

impl DiskRates {
    pub(super) fn update(&mut self, disks: &mut [DiskSnapshot], elapsed_seconds: f64) {
        // Replace, rather than extend, so removal/reinsertion gets a fresh baseline and
        // the map stays bounded by the same collection limit as the outgoing report.
        let mut current = HashMap::with_capacity(disks.len());
        for disk in disks {
            let id = (
                disk.name.clone(),
                disk.mount_point.clone(),
                disk.file_system.clone(),
            );
            let counters = (disk.read_bytes_total, disk.written_bytes_total);
            let deltas = self.previous.get(&id).and_then(|previous| {
                Some((
                    counters.0.checked_sub(previous.0)?,
                    counters.1.checked_sub(previous.1)?,
                ))
            });
            let (read, written) = deltas.unwrap_or_default();
            disk.read_bytes_per_second = super::per_second(read, elapsed_seconds);
            disk.written_bytes_per_second = super::per_second(written, elapsed_seconds);
            current.insert(id, counters);
        }
        self.previous = current;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(read: u64, written: u64) -> DiskSnapshot {
        DiskSnapshot {
            name: "disk-a".into(),
            mount_point: "/media/disk-a".into(),
            file_system: "ext4".into(),
            total_bytes: 1_000_000_000_000,
            available_bytes: 500_000_000_000,
            read_bytes_total: read,
            written_bytes_total: written,
            read_bytes_per_second: 123.0,
            written_bytes_per_second: 456.0,
            is_read_only: false,
        }
    }

    #[test]
    fn newly_discovered_volume_does_not_report_lifetime_io_as_one_interval() {
        let mut rates = DiskRates::default();
        let mut values = [disk(512_000_000_000, 1_024_000_000_000)];
        rates.update(&mut values, 10.0);
        assert_eq!(values[0].read_bytes_total, 512_000_000_000);
        assert_eq!(values[0].read_bytes_per_second, 0.0);
        assert_eq!(values[0].written_bytes_per_second, 0.0);
        values[0].read_bytes_total += 1000;
        values[0].written_bytes_total += 2000;
        rates.update(&mut values, 10.0);
        assert_eq!(values[0].read_bytes_per_second, 100.0);
        assert_eq!(values[0].written_bytes_per_second, 200.0);
    }

    #[test]
    fn removal_reinsertion_and_counter_reset_reprime_the_baseline() {
        let mut rates = DiskRates::default();
        let mut values = [disk(u64::MAX - 100, u64::MAX - 100)];
        rates.update(&mut values, 10.0);
        values[0] = disk(50, 80); // Counter reset or wrap is not new I/O.
        rates.update(&mut values, 10.0);
        assert_eq!(values[0].read_bytes_per_second, 0.0);
        assert_eq!(values[0].written_bytes_per_second, 0.0);
        values[0] = disk(150, 280);
        rates.update(&mut values, 2.0);
        assert_eq!(values[0].read_bytes_per_second, 50.0);
        assert_eq!(values[0].written_bytes_per_second, 100.0);
        rates.update(&mut [], 10.0);
        assert!(rates.previous.is_empty());
        values[0] = disk(500_000, 900_000);
        rates.update(&mut values, 10.0);
        assert_eq!(values[0].read_bytes_per_second, 0.0);
        assert_eq!(values[0].written_bytes_per_second, 0.0);
    }

    #[test]
    fn remounted_volume_and_unchanged_counters_do_not_repeat_a_delta() {
        let mut rates = DiskRates::default();
        let mut values = [disk(100, 200)];
        rates.update(&mut values, 10.0);
        values[0] = disk(1100, 2200);
        rates.update(&mut values, 10.0);
        rates.update(&mut values, 10.0);
        assert_eq!(values[0].read_bytes_per_second, 0.0);
        values[0].mount_point = "/media/new-mount".into();
        values[0].read_bytes_total += 10_000;
        rates.update(&mut values, 10.0);
        assert_eq!(values[0].read_bytes_per_second, 0.0);
        assert_eq!(rates.previous.len(), 1);
    }
}
