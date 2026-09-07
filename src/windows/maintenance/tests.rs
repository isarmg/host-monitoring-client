#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_sid_is_stable_and_has_five_subauthorities() {
        let sid = service_sid_string().unwrap();
        assert!(sid.starts_with("S-1-5-80-"));
        assert_eq!(sid.split('-').count(), 9);
        assert_eq!(sid, service_sid_string().unwrap());
    }
}
