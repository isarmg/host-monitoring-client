mod monitor_app;

fn main() -> anyhow::Result<()> {
    monitor_app::entry()
}
