//! A supervised process for controller fixtures, with no sockets or routing.
#![allow(unreachable_pub, clippy::redundant_pub_crate)]

use cvt_core::AppPaths;
use cvt_core::mihomo::Supervisor;
use std::os::unix::fs::PermissionsExt as _;
use std::time::{Duration, Instant};

pub struct ManagedProcess(Supervisor);

impl ManagedProcess {
    pub fn start(paths: &AppPaths, config: &str) -> Self {
        let supervisor = Supervisor::new(paths.clone());
        let fixture = paths.core_dir().join("process-fixture.py");
        std::fs::write(
            &fixture,
            "#!/usr/bin/python3\nimport time\nwhile True: time.sleep(60)\n",
        )
        .unwrap();
        std::fs::set_permissions(&fixture, std::fs::Permissions::from_mode(0o700)).unwrap();
        paths.write_atomic(&paths.runtime_config(), config).unwrap();
        supervisor.start(&fixture, &paths.runtime_config()).unwrap();
        let process = Self(supervisor);
        let deadline = Instant::now() + Duration::from_secs(2);
        while process.0.check_health().is_err() {
            assert!(
                Instant::now() < deadline,
                "supervised fixture did not start"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        process
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        let _ = self.0.stop();
    }
}
