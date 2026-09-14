fn main() {
    println!("cargo:rerun-if-env-changed=HOST_MONITOR_BUILD_SHA");
    let revision = std::env::var("HOST_MONITOR_BUILD_SHA").unwrap_or_else(|_| "unknown".into());
    if revision != "unknown"
        && (revision.len() != 40
            || !revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    {
        panic!("HOST_MONITOR_BUILD_SHA must be a full lowercase Git commit SHA");
    }
    println!("cargo:rustc-env=HOST_MONITOR_BUILD_SHA={revision}");
}
