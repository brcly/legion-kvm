#[cfg(unix)]
mod application;
#[cfg(unix)]
mod audio_diagnostics;
#[cfg(unix)]
mod cpu_adapter_control;
#[cfg(unix)]
mod cpu_profile;
#[cfg(unix)]
mod nvidia_runtime;
#[cfg(unix)]
mod wine_compat;

#[cfg(unix)]
mod linux {
    use anyhow::{Context, Result, ensure};
    use serde::Deserialize;
    use std::{
        ffi::CString,
        fs,
        io::Write,
        os::unix::{fs::PermissionsExt, process::CommandExt},
        path::Path,
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    #[derive(Default, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum VulkanDriver {
        #[default]
        Nvidia,
        Radv,
    }
    impl VulkanDriver {
        fn icd(&self) -> &'static str {
            match self {
                Self::Nvidia => "/usr/share/vulkan/icd.d/nvidia_icd.json",
                Self::Radv => "/usr/share/vulkan/icd.d/radeon_icd.json",
            }
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Launch {
        proton_path: String,
        uid: u32,
        gid: u32,
        #[serde(default)]
        vulkan_driver: VulkanDriver,
        #[serde(default)]
        debug: bool,
        #[serde(default)]
        thread_snapshots: bool,
        #[serde(default)]
        presentation_telemetry: bool,
        #[serde(default)]
        thread_exit_trace: bool,
        #[serde(default)]
        exception_trace: bool,
        #[serde(default)]
        display_backend: DisplayBackend,
        #[serde(default)]
        forward_wait: ForwardWait,
        #[serde(default)]
        explicit_sync: bool,
        #[serde(default = "default_forward_idle_retry_us")]
        forward_idle_retry_us: u64,
        #[serde(default)]
        mangohud: bool,
        #[serde(default = "default_mangohud_config")]
        mangohud_config: String,
        #[serde(default = "nvidia_features_default")]
        nvidia_features: bool,
        #[serde(default)]
        nvidia_diagnostics: bool,
        #[serde(default)]
        desktop_audio: bool,
        #[serde(default)]
        audio_diagnostics: bool,
        #[serde(default)]
        audio_trace: bool,
        #[serde(default)]
        vkd3d_config: String,
        #[serde(default)]
        no_uvm_layer: bool,
        #[serde(default)]
        semaphore_probe: bool,
        #[serde(default)]
        attach_submit_fence: bool,
        #[serde(default)]
        semaphore_spin_us: u32,
        #[serde(default)]
        semaphore_memory_probe: bool,
        #[serde(default)]
        gpu_clock_probe: bool,
        #[serde(default)]
        cpu_profile_markers_only: bool,
        #[serde(default)]
        queue_profile: bool,
        #[serde(default)]
        call_probe: bool,
        #[serde(default)]
        present_cpu_profile: bool,
        #[serde(default)]
        queue_cpu_profile: bool,
        #[serde(default)]
        queue_endpoint_profile: bool,
        #[serde(default)]
        queue_latency_profile: Option<QueueLatency>,
        #[serde(default)]
        queue_cycle_profile: bool,
        #[serde(default)]
        queue_profile_include_kernel: bool,
        #[serde(default)]
        synchronization_trace: bool,
        #[serde(default)]
        native_forward: bool,
        #[serde(default)]
        driver_yield: Option<DriverYield>,
        #[serde(default)]
        native_semsurf_control: Option<NativeSemsurf>,
        #[serde(default)]
        syscall_counts: bool,
        #[serde(default)]
        guest_wakeups: Option<GuestWakeups>,
        #[serde(default)]
        driver_poll_spin_us: u32,
        #[serde(default)]
        driver_rpc_spin_us: Option<u32>,
        #[serde(default)]
        queue_placement: Option<QueuePlacement>,
        #[serde(default)]
        server_trace: bool,
        #[serde(default)]
        external_benchmark: bool,
        #[serde(default)]
        wine_overlay: Option<crate::wine_compat::Overlay>,
        #[serde(default)]
        ephemeral_wine_overlay: bool,
        #[serde(default)]
        host_paths: Option<HostPaths>,
        #[serde(default)]
        environment: std::collections::BTreeMap<String, String>,
        #[serde(default)]
        startup_contracts: bool,
        #[serde(default)]
        cpu_protocol_audit: bool,
        #[serde(default)]
        cpu_adapter: bool,
        #[serde(default)]
        kuser_worker: bool,
        #[serde(default)]
        nt_syscall_audit: bool,
        #[serde(default)]
        wine_dispatch_audit: bool,
        #[serde(default)]
        resolver_bridge: bool,
        #[serde(default)]
        resolver_contracts: bool,
        #[serde(default)]
        application: Option<crate::application::Application>,
    }

    #[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    enum DisplayBackend {
        #[default]
        Auto,
        Wayland,
        Xwayland,
    }

    #[derive(Clone, Copy, Default, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum ForwardWait {
        #[default]
        Spin,
        Events,
        Hybrid,
    }

    impl ForwardWait {
        fn as_str(self) -> &'static str {
            match self {
                Self::Spin => "spin",
                Self::Events => "events",
                Self::Hybrid => "hybrid",
            }
        }
    }

    fn default_forward_idle_retry_us() -> u64 {
        1000
    }

    fn default_mangohud_config() -> String {
        "fps,frametime,frame_timing,cpu_stats,ram,resolution,gpu_stats=0,position=top-left".into()
    }
    fn nvidia_features_default() -> bool {
        true
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct QueueLatency {
        opcode: u32,
        stride: u32,
    }

    #[derive(Deserialize)]
    enum DriverYield {
        #[serde(rename = "USLEEP")]
        Usleep,
        #[serde(rename = "NOTHING")]
        Nothing,
    }
    #[derive(Deserialize)]
    enum NativeSemsurf {
        #[serde(rename = "pass")]
        Pass,
        #[serde(rename = "disable")]
        Disable,
    }
    #[derive(Deserialize)]
    enum GuestWakeups {
        #[serde(rename = "queued")]
        Queued,
        #[serde(rename = "direct")]
        Direct,
    }
    #[derive(Deserialize)]
    enum QueuePlacement {
        #[serde(rename = "original")]
        Original,
        #[serde(rename = "colocated")]
        Colocated,
        #[serde(rename = "server-original")]
        ServerOriginal,
        #[serde(rename = "server-colocated")]
        ServerColocated,
    }
    impl DriverYield {
        fn as_str(&self) -> &'static str {
            match self {
                Self::Usleep => "USLEEP",
                Self::Nothing => "NOTHING",
            }
        }
    }

    struct OwnedServer(Child);
    fn clock_environment() -> serde_json::Value {
        use std::os::unix::fs::FileExt;
        let msr = |offset| {
            let mut bytes = [0; 8];
            match fs::File::open("/dev/cpu/0/msr")
                .and_then(|file| file.read_exact_at(&mut bytes, offset))
            {
                Ok(()) => serde_json::json!({"value":u64::from_le_bytes(bytes)}),
                Err(error) => serde_json::json!({"unavailable":error.to_string()}),
            }
        };
        let read = |name| {
            fs::read_to_string(format!(
                "/sys/devices/system/clocksource/clocksource0/{name}"
            ))
            .ok()
            .map(|v| v.trim().to_owned())
        };
        #[cfg(target_arch = "x86_64")]
        let kvm_features = {
            let present = std::arch::x86_64::__cpuid(1).ecx & (1 << 31) != 0;
            present.then(|| std::arch::x86_64::__cpuid(0x40000001).eax)
        };
        #[cfg(not(target_arch = "x86_64"))]
        let kvm_features: Option<u32> = None;
        serde_json::json!({"schema":1,"current_clocksource":read("current_clocksource"),"available_clocksource":read("available_clocksource"),"hypervisor_feature_leaf_eax":kvm_features,
            "mtrr_def_type":msr(0x2ff),"pat":msr(0x277),"mtrr_ranges":fs::read_to_string("/proc/mtrr").ok()})
    }
    impl Drop for OwnedServer {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// Read-only snapshots inside the already-verified disposable guest only.
    struct ThreadSnapshots {
        stop: std::sync::mpsc::Sender<()>,
        worker: Option<std::thread::JoinHandle<Result<()>>>,
    }
    impl ThreadSnapshots {
        fn counter_page() -> Option<serde_json::Value> {
            use std::os::unix::fs::FileExt;
            let request: serde_json::Value =
                serde_json::from_slice(&fs::read("/frames/semaphore-probe.counter.json").ok()?)
                    .ok()?;
            let pid = u32::try_from(request["pid"].as_u64()?).ok()?;
            let address = request["address"].as_u64()?;
            let process = format!("/proc/{pid}");
            if pid < 2
                || !fs::read_link(format!("{process}/exe"))
                    .ok()?
                    .to_string_lossy()
                    .contains("wine")
            {
                return None;
            }
            let mut bytes = [0_u8; 8];
            fs::File::open(format!("{process}/pagemap"))
                .ok()?
                .read_exact_at(&mut bytes, (address / 4096).checked_mul(8)?)
                .ok()?;
            let entry = u64::from_le_bytes(bytes);
            let frame = entry & ((1_u64 << 55) - 1);
            let physical = (entry >> 63 != 0 && frame != 0).then(|| frame * 4096 + address % 4096);
            let mut devices = Vec::new();
            for device in fs::read_dir("/sys/bus/virtio/drivers/virtio-gpu-nv")
                .into_iter()
                .flatten()
                .flatten()
            {
                if !device.file_name().to_string_lossy().starts_with("virtio") {
                    continue;
                }
                let Ok(path) = device.path().canonicalize() else {
                    continue;
                };
                let Some(pci) = path.parent() else {
                    continue;
                };
                devices.push(serde_json::json!({"pci":pci.file_name().map(|v|v.to_string_lossy()),"resource":fs::read_to_string(pci.join("resource")).ok(),"config":fs::read(pci.join("config")).ok().map(|v|v.into_iter().take(256).collect::<Vec<_>>())}));
            }
            Some(
                serde_json::json!({"pid":pid,"address":address,"pagemap_entry":entry,"physical":physical,"devices":devices}),
            )
        }

        fn poll_descriptors(process: &Path, syscall: &str) -> Vec<serde_json::Value> {
            use std::os::unix::fs::FileExt;
            let fields: Vec<_> = syscall.split_whitespace().collect();
            if fields.len() < 3 || fields[0].parse::<i64>().ok() != Some(libc::SYS_poll) {
                return Vec::new();
            }
            let parse = |text: &str| {
                text.strip_prefix("0x")
                    .and_then(|v| u64::from_str_radix(v, 16).ok())
            };
            let (Some(address), Some(count)) = (parse(fields[1]), parse(fields[2])) else {
                return Vec::new();
            };
            if !(1..=8).contains(&count) {
                return Vec::new();
            }
            let mut data = [0_u8; 64];
            let Ok(memory) = fs::File::open(process.join("mem")) else {
                return Vec::new();
            };
            // Read only the blocked poll's bounded descriptor array in this owned guest process.
            if memory
                .read_exact_at(&mut data[..count as usize * 8], address)
                .is_err()
            {
                return Vec::new();
            }
            data[..count as usize*8].as_chunks::<8>().0.iter().map(|entry| {
                let fd = i32::from_ne_bytes([entry[0],entry[1],entry[2],entry[3]]);
                serde_json::json!({"fd":fd,"events":i16::from_ne_bytes([entry[4],entry[5]]),"revents":i16::from_ne_bytes([entry[6],entry[7]]),"target":fs::read_link(process.join("fd").join(fd.to_string())).ok().map(|v|v.to_string_lossy().into_owned()),"info":fs::read_to_string(process.join("fdinfo").join(fd.to_string())).ok()})
            }).collect()
        }

        fn start(exit_trace: bool) -> Result<Self> {
            if exit_trace {
                enable_guest_trace()?;
            }
            let mut output = fs::File::create("/frames/proton-threads.jsonl")?;
            let (stop, receive) = std::sync::mpsc::channel();
            let worker = std::thread::Builder::new().name("proton-snapshot".into()).spawn(move || {
                let start = Instant::now();
                let mut traced_owner = 0;
                let mut captured_owner = 0;
                loop {
                    let mut threads = Vec::new();
                    let owner = fs::read("/frames/cpu-adapter-state.bin").ok().and_then(|data| {
                        data.get(8..16).and_then(|v|v.try_into().ok()).map(u64::from_le_bytes)
                    }).unwrap_or(0);
                    if owner>0 && captured_owner!=owner
                        && let Ok(maps)=fs::read(format!("/proc/{owner}/maps")) {
                            fs::write("/frames/application-maps.txt",maps)?;
                            captured_owner=owner;
                    }
                    if exit_trace && owner>0 && traced_owner==0 {
                        let trace=Path::new("/sys/kernel/tracing");
                        fs::write(trace.join("events/raw_syscalls/sys_enter/filter"),format!("common_pid == {owner} && (id == 60 || id == 231)"))?;
                        fs::write(trace.join("options/userstacktrace"),"1")?;
                        fs::write(trace.join("events/raw_syscalls/sys_enter/enable"),"1")?;
                        if let Ok(maps)=fs::read(format!("/proc/{owner}/maps")) {fs::write("/frames/thread-exit-maps.txt",maps)?;}
                        traced_owner=owner;
                    }
                    if traced_owner>0 {
                        fs::write("/frames/thread-exit-trace.txt",fs::read("/sys/kernel/tracing/trace")?)?;
                    }
                    for process in fs::read_dir("/proc")? {
                        let process = process?;
                        let executable = fs::read_link(process.path().join("exe")).unwrap_or_default();
                        let executable = executable.to_string_lossy();
                        // Wine may replace /proc/exe with the Windows image after startup.
                        if !executable.contains("wine") && !executable.ends_with(".exe")
                            && process.file_name().to_string_lossy().parse::<u64>().ok()!=Some(owner)
                            && !executable.ends_with("/legion/compositor") {continue;}
                        let Ok(tasks) = fs::read_dir(process.path().join("task")) else {continue;};
                        for task in tasks.flatten().take(256) {
                            let read = |name| fs::read_to_string(task.path().join(name)).unwrap_or_else(|e|format!("unavailable: {e}"));
                            let syscall = read("syscall");
                            let descriptors = Self::poll_descriptors(&process.path(),&syscall);
                            let comm=read("comm");
                            let wait=if comm.contains("GameThread") || comm.contains("RenderThread") || comm.contains("RHI") {
                                Self::windows_wait(&process.path(),&syscall)
                            }else{Vec::new()};
                            threads.push(serde_json::json!({"pid":process.file_name().to_string_lossy(),"tid":task.file_name().to_string_lossy(),"exe":executable,"comm":comm,"wchan":read("wchan"),"syscall":syscall,"poll_descriptors":descriptors,"windows_wait":wait,"stack":read("stack")}));
                        }
                    }
                    output.write_all(&serde_json::to_vec(&serde_json::json!({"elapsed_ms":start.elapsed().as_millis(),"threads":threads,"counter_page":Self::counter_page(),
                        "owner":owner,"owner_status":if owner>0 {fs::read_to_string(format!("/proc/{owner}/status")).ok()} else {None},
                        "owner_exe":if owner>0 {fs::read_link(format!("/proc/{owner}/exe")).ok().map(|v|v.to_string_lossy().into_owned())} else {None},
                        "owner_limits":if owner>0 {fs::read_to_string(format!("/proc/{owner}/limits")).ok()} else {None},
                        "owner_open_fds":if owner>0 {fs::read_dir(format!("/proc/{owner}/fd")).ok().map(|entries|entries.filter_map(Result::ok).count())} else {None}
                    }))?)?;
                    output.write_all(b"\n")?;
                    match receive.recv_timeout(Duration::from_secs(1)) {
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        _ => break,
                    }
                }
                Ok(())
            })?;
            Ok(Self {
                stop,
                worker: Some(worker),
            })
        }

        // Diagnostic reader for the pinned GE11 x64 syscall_frame layout in signal_x86_64.c.
        fn windows_wait(process: &Path, syscall: &str) -> Vec<serde_json::Value> {
            use std::os::unix::fs::FileExt;
            let fields: Vec<_> = syscall.split_whitespace().collect();
            let parse = |s: &str| {
                s.strip_prefix("0x")
                    .and_then(|v| u64::from_str_radix(v, 16).ok())
            };
            if fields.len() != 9 || fields[0].parse::<u32>().is_err() {
                return Vec::new();
            }
            let Some(sp) = parse(fields[7]) else {
                return Vec::new();
            };
            let page = sp & !4095;
            let Ok(memory) = fs::File::open(process.join("mem")) else {
                return Vec::new();
            };
            // Native calls may place their C stack one or more pages below Wine's retained Windows syscall frame.
            let mut bytes = [0_u8; 16384];
            let Ok(length) = memory.read_at(&mut bytes, page) else {
                return Vec::new();
            };
            if length < 0xc0 {
                return Vec::new();
            }
            let mut result = Vec::new();
            let first = ((sp - page) as usize + 15) & !15;
            for at in (first..=length - 0xc0).step_by(16) {
                let q = |offset| {
                    u64::from_le_bytes(
                        bytes[at + offset..at + offset + 8]
                            .try_into()
                            .expect("bounded syscall frame"),
                    )
                };
                let (rip, rsp) = (q(0x70), q(0x88));
                if q(0x78) & 0xffff != 0x33
                    || q(0x90) & 0xffff != 0x2b
                    || q(0x80) & 2 == 0
                    || !(0x10000..1_u64 << 47).contains(&rip)
                    || !(0x10000..1_u64 << 47).contains(&rsp)
                {
                    continue;
                }
                let mut stack = [0_u8; 2048];
                let words = if memory.read_exact_at(&mut stack, rsp).is_ok() {
                    stack
                        .as_chunks::<8>()
                        .0
                        .iter()
                        .map(|b| u64::from_le_bytes(*b))
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                let incomplete = q(0xb0) >> 32 & 0x20000 != 0;
                result.push(
                    serde_json::json!({"frame":page+at as u64,"service":(!incomplete).then_some(q(0xb0)&0xffffffff),
                    "kind":if incomplete {"unix_call"}else{"nt_syscall"},
                    "restore_flags":q(0xb0)>>32,
                    "rip":rip,"rsp":rsp,"rbp":q(0x98),"saved_rdx":(!incomplete).then_some(q(0x18)),
                    "nonvolatile":{"rbx":q(0x8),"rsi":q(0x20),"rdi":q(0x28),"r12":q(0x50),"r13":q(0x58),"r14":q(0x60),"r15":q(0x68)},
                    "stack_words":words}),
                );
            }
            result
        }
    }
    impl Drop for ThreadSnapshots {
        fn drop(&mut self) {
            let _ = self.stop.send(());
            if let Some(worker) = self.worker.take() {
                match worker.join() {
                    Ok(Ok(())) => {}
                    result => eprintln!("thread snapshot stopped: {result:?}"),
                }
            }
        }
    }

    fn bind(source: &str, target: &str) -> Result<()> {
        bind_flags(source, target, libc::MS_BIND)
    }

    fn bind_flags(source: &str, target: &str, flags: libc::c_ulong) -> Result<()> {
        let source = CString::new(source)?;
        let target = CString::new(target)?;
        // SAFETY: guest-only caller verifies the disposable PID1 identity before any mount.
        let rc = unsafe {
            libc::mount(
                source.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                flags,
                std::ptr::null(),
            )
        };
        ensure!(
            rc == 0,
            "guest bind mount {} to {} failed: {}",
            source.to_string_lossy(),
            target.to_string_lossy(),
            std::io::Error::last_os_error()
        );
        Ok(())
    }

    fn bind_shared_proton(source: &Path, target: &Path) -> Result<()> {
        // Keep the tool root private so dist.lock can exist even when absent from the installed tree.
        for entry in fs::read_dir(source).context("list selected Proton tool")? {
            let entry = entry?;
            if entry.file_name() == "dist.lock" {
                continue;
            }
            let destination = target.join(entry.file_name());
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                std::os::unix::fs::symlink(fs::read_link(entry.path())?, &destination)?;
                continue;
            }
            if kind.is_dir() {
                fs::create_dir(&destination)?;
            } else {
                ensure!(kind.is_file(), "unsupported entry in selected Proton tool");
                fs::File::create(&destination)?;
            }
            bind(
                entry.path().to_str().context("non-UTF8 Proton entry")?,
                destination
                    .to_str()
                    .context("non-UTF8 Proton destination")?,
            )?;
        }
        // bind(2) needs a destination even if Proton has never created a lock.
        fs::File::create(target.join("dist.lock"))?;
        Ok(())
    }

    fn chown(path: &Path, uid: u32, gid: u32) -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let name = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: path is NUL terminated.
        let rc = unsafe { libc::chown(name.as_ptr(), uid, gid) };
        ensure!(
            rc == 0,
            "chown {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
        Ok(())
    }

    fn enable_guest_trace() -> Result<()> {
        if !Path::new("/sys/kernel/tracing/trace").exists() {
            fs::create_dir_all("/sys/kernel/tracing")?;
            // SAFETY: called only after verifying this disposable guest's PID1 and explicit diagnostic configuration.
            let result = unsafe {
                libc::mount(
                    c"tracefs".as_ptr(),
                    c"/sys/kernel/tracing".as_ptr(),
                    c"tracefs".as_ptr(),
                    0,
                    std::ptr::null(),
                )
            };
            ensure!(
                result == 0,
                "mount guest tracefs: {}",
                std::io::Error::last_os_error()
            );
        }
        fs::write("/sys/kernel/tracing/trace", [])?;
        fs::write("/sys/kernel/tracing/tracing_on", "1")?;
        Ok(())
    }

    /// Called only after verifying the disposable guest identity, before any children drop privileges.
    fn prepare_guest_file_limits() -> Result<()> {
        let mut previous = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: complete writable output for this guest process's limits.
        let read = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut previous) };
        ensure!(
            read == 0,
            "read guest descriptor limit: {}",
            std::io::Error::last_os_error()
        );
        let maximum: libc::rlim_t = fs::read_to_string("/proc/sys/fs/nr_open")?
            .trim()
            .parse()
            .context("parse guest kernel descriptor ceiling")?;
        ensure!(maximum > 0, "invalid guest kernel descriptor ceiling");
        let requested = libc::rlimit {
            rlim_cur: maximum,
            rlim_max: maximum,
        };
        // SAFETY: changes only this already-verified guest process and its future children.
        let changed = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &requested) };
        ensure!(
            changed == 0,
            "prepare guest descriptor limit: {}",
            std::io::Error::last_os_error()
        );
        fs::write(
            "/frames/guest-file-limits.json",
            serde_json::to_vec(&serde_json::json!({
                "schema":1,"scope":"guest_process_tree","resource":"nofile",
                "before":{"soft":previous.rlim_cur,"hard":previous.rlim_max},
                "after":{"soft":requested.rlim_cur,"hard":requested.rlim_max}
            }))?,
        )?;
        Ok(())
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct HostPaths {
        home: String,
        proton: String,
        application: String,
        compatdata: String,
        #[serde(default)]
        wine_prefix: Option<String>,
    }

    fn host_alias(source: &str, destination: &str) -> Result<()> {
        use std::path::Component;
        let path = Path::new(destination);
        ensure!(
            path.is_absolute()
                && path.components().count() > 2
                && path
                    .components()
                    .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
            "invalid original host path"
        );
        ensure!(
            ![
                "/dev", "/proc", "/sys", "/run", "/usr", "/etc", "/opt", "/bin", "/sbin", "/lib",
                "/lib64"
            ]
            .iter()
            .any(|root| path.starts_with(root)),
            "original host path overlaps guest system directories"
        );
        fs::create_dir_all(path)?;
        bind(source, destination)
    }

    pub fn run() -> Result<()> {
        let native = std::env::args().any(|arg| arg == "--native");
        let launch: Launch = serde_json::from_slice(&fs::read("/usr/share/legion/launch.json")?)?;
        ensure!(
            !launch.thread_exit_trace
                || (!native && launch.application.is_some() && launch.thread_snapshots),
            "thread exit tracing requires the guest application and thread snapshots"
        );
        ensure!(
            launch.application.is_none()
                || (!native && !launch.external_benchmark && !launch.cpu_protocol_audit),
            "application launch requires the guest and cannot run the owned Windows audit"
        );
        ensure!(
            launch.wine_overlay.is_none() || (!native && !launch.external_benchmark),
            "Wine overlay is restricted to the owned disposable guest fixture"
        );
        ensure!(
            !launch.startup_contracts || !launch.external_benchmark,
            "startup contracts require the owned Windows fixture"
        );
        ensure!(
            !launch.cpu_protocol_audit || !launch.external_benchmark,
            "CPU/protocol audit requires the owned Windows fixture"
        );
        ensure!(
            !launch.cpu_adapter || (!native && !launch.external_benchmark),
            "CPU adapter is restricted to the owned disposable guest fixture"
        );
        ensure!(
            !launch.kuser_worker
                || (launch.cpu_adapter
                    && (launch.cpu_protocol_audit || launch.application.is_some())),
            "KUSER worker requires the guest CPU adapter and an audit or application"
        );
        ensure!(
            !launch.nt_syscall_audit || launch.kuser_worker,
            "NT syscall audit requires the owned guest KUSER/CPU controls"
        );
        ensure!(
            !launch.wine_dispatch_audit || launch.nt_syscall_audit,
            "Wine dispatcher audit requires the owned direct NT controls"
        );
        ensure!(
            !launch.resolver_bridge || launch.cpu_adapter,
            "resolver bridge requires the guest CPU adapter"
        );
        ensure!(
            !launch.resolver_contracts || (launch.resolver_bridge && launch.application.is_none()),
            "resolver contracts require the owned guest fixture and bridge"
        );
        ensure!(
            launch.uid != 0 && launch.gid != 0,
            "benchmark must run as an unprivileged user"
        );
        ensure!(
            Path::new(&launch.proton_path).is_absolute(),
            "Proton path must be absolute"
        );
        if !native {
            ensure!(
                fs::read_link("/proc/1/exe")? == Path::new("/init")
                    && fs::read_to_string("/proc/cmdline")?
                        .split_whitespace()
                        .any(|a| a == "legion.vulkan=1"),
                "mount setup is restricted to the disposable Legion guest"
            );
            prepare_guest_file_limits()?;
            if let Some(spin_us) = launch.driver_rpc_spin_us {
                ensure!(
                    spin_us <= 100,
                    "driver request polling exceeds 100 microseconds"
                );
                let path = "/sys/module/virtio_gpu_nv/parameters/rpc_spin_us";
                let before = fs::read_to_string(path)?;
                fs::write(path, spin_us.to_string())?;
                let after = fs::read_to_string(path)?;
                ensure!(
                    after.trim().parse::<u32>()? == spin_us,
                    "driver request polling setting did not match"
                );
                fs::write(
                    "/frames/driver-rpc-control.json",
                    serde_json::to_vec_pretty(
                        &serde_json::json!({"before":before,"after":after,"guest_only":true,"host_changed":false}),
                    )?,
                )?;
            }
            if launch.driver_poll_spin_us != 0 {
                ensure!(
                    launch.driver_poll_spin_us <= 100,
                    "driver polling exceeds 100 microseconds"
                );
                let path = "/sys/module/virtio_gpu_nv/parameters/poll_spin_us";
                let before = fs::read_to_string(path)?;
                fs::write(path, launch.driver_poll_spin_us.to_string())?;
                let after = fs::read_to_string(path)?;
                ensure!(
                    after.trim().parse::<u32>()? == launch.driver_poll_spin_us,
                    "driver polling setting did not match"
                );
                fs::write(
                    "/frames/driver-poll-control.json",
                    serde_json::to_vec_pretty(
                        &serde_json::json!({"before":before,"after":after,"guest_only":true,"host_changed":false}),
                    )?,
                )?;
            }
            if let Some(paths) = &launch.host_paths {
                host_alias("/run/legion-shares/proton-home", &paths.home)?;
                host_alias("/run/legion-shares/application", &paths.application)?;
                host_alias("/run/legion-shares/proton-compatdata", &paths.compatdata)?;
                if let Some(prefix) = &paths.wine_prefix {
                    host_alias("/run/legion-shares/wine-prefix", prefix)?;
                }
            }
            if Path::new("/run/legion-shares/nvidia-wine").is_dir() {
                bind("/run/legion-shares/nvidia-wine", "/usr/lib/nvidia/wine")?;
            }
            fs::create_dir_all(&launch.proton_path)?;
            let shared_tool = Path::new("/run/legion-shares/proton-tool");
            if shared_tool.is_dir() {
                ensure!(
                    shared_tool.join("proton").is_file(),
                    "shared Proton tool has no proton entrypoint"
                );
                if let Some(paths) = &launch.host_paths {
                    ensure!(
                        paths.proton == launch.proton_path,
                        "original Proton path mismatch"
                    );
                    let private = Path::new("/opt/legion-private-proton");
                    fs::create_dir_all(private)?;
                    bind_shared_proton(shared_tool, private)?;
                    bind_flags(
                        private.to_str().context("invalid private Proton path")?,
                        &launch.proton_path,
                        libc::MS_BIND | libc::MS_REC,
                    )?;
                } else {
                    bind_shared_proton(shared_tool, Path::new(&launch.proton_path))?;
                }
            } else {
                bind("/usr/share/legion-proton", &launch.proton_path)?;
            }
            fs::create_dir_all("/etc")?;
            if Path::new("/usr/etc/fonts").is_dir() {
                fs::create_dir_all("/etc/fonts")?;
                bind("/usr/etc/fonts", "/etc/fonts")?;
            }
            for name in ["passwd", "group", "nsswitch.conf", "hosts", "machine-id"] {
                fs::copy(
                    Path::new("/usr/share/legion/etc").join(name),
                    Path::new("/etc").join(name),
                )?;
            }
            let runtime = format!("/run/user/{}", launch.uid);
            fs::create_dir_all(&runtime)?;
            fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))?;
            chown(Path::new(&runtime), launch.uid, launch.gid)?;
            for node in [
                "/dev/nvidia0",
                "/dev/nvidiactl",
                "/dev/nvidia-uvm",
                "/dev/ntsync",
            ] {
                if Path::new(node).exists() {
                    fs::set_permissions(node, fs::Permissions::from_mode(0o666))?;
                }
            }
            for node in fs::read_dir("/dev/dri")? {
                let node = node?;
                if node.file_name().to_string_lossy().starts_with("renderD")
                    || (Path::new("/frames/kms-presentation.json").is_file()
                        && node.file_name().to_string_lossy().starts_with("card"))
                {
                    fs::set_permissions(node.path(), fs::Permissions::from_mode(0o666))?;
                }
            }
        }
        for directory in ["home", "compatdata", "steam", "cache"] {
            let path = Path::new("/frames").join(directory);
            fs::create_dir_all(&path)?;
            if !native {
                chown(&path, launch.uid, launch.gid)?;
            }
        }
        if launch.desktop_audio {
            ensure!(!native, "guest ALSA playback configuration requires a VM");
            let mut playback = None;
            let mut devices = Vec::new();
            for entry in fs::read_dir("/dev/snd").context(
                "desktop audio requires the guest sound modules and a virtual playback device",
            )? {
                let entry = entry?;
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("controlC")
                    || name == "timer"
                    || (name.starts_with("pcmC") && name.ends_with('p'))
                {
                    chown(&entry.path(), launch.uid, launch.gid)?;
                    fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o600))?;
                    if let Some((card, device)) = name
                        .strip_prefix("pcmC")
                        .and_then(|s| s.strip_suffix('p'))
                        .and_then(|s| s.split_once('D'))
                    {
                        let card: u32 = card.parse().context("invalid guest audio card")?;
                        let device: u32 =
                            device.parse().context("invalid guest playback device")?;
                        playback.get_or_insert((card, device));
                    }
                    devices.push(name.into_owned());
                }
            }
            let (card, device) = playback.context("guest sound device has no playback PCM")?;
            // Unknown ALSA cards default to exclusive plughw playback.
            fs::write(
                "/tmp/legion-audio.conf",
                format!(
                    concat!(
                        "</usr/share/alsa/alsa.conf>\ndefaults.pcm.!ipc_gid {}\n",
                        "pcm.legion_mix {{\n type dmix\n ipc_key 1279741761\n",
                        " ipc_key_add_uid true\n ipc_perm 0600\n",
                        " slave {{ pcm \"hw:{},{}\" format S16_LE rate 48000 channels 2",
                        " period_size 480 buffer_size 4800 }}\n}}\n",
                        "pcm.!default {{ type plug slave.pcm \"legion_mix\" }}\n"
                    ),
                    launch.gid, card, device
                ),
            )?;
            ensure!(
                !launch.application.as_ref().is_some_and(|app| app
                    .arguments
                    .iter()
                    .any(|arg| arg.eq_ignore_ascii_case("-nosound"))),
                "remove -nosound when desktop audio is enabled"
            );
            fs::write(
                "/frames/audio-runtime.json",
                serde_json::to_vec_pretty(&serde_json::json!({
                "schema":1,"backend":"guest ALSA / virtual sound card / host desktop audio server",
                    "playback_devices":devices,"cards":fs::read_to_string("/proc/asound/cards")?,"microphone_exposed":false
                }))?,
            )?;
            let status = Command::new("/usr/share/legion/audio-runtime")
                .arg("--initialize-mixer")
                .arg(card.to_string())
                .env_clear()
                .env("LD_LIBRARY_PATH", "/usr/lib:/usr/lib/x86_64-linux-gnu")
                .uid(launch.uid)
                .gid(launch.gid)
                .stdin(Stdio::null())
                .stdout(fs::File::create("/frames/audio-mixer.json")?)
                .stderr(fs::File::create("/frames/audio-mixer.stderr")?)
                .status()
                .context("initialize virtual playback mixer")?;
            ensure!(
                status.success(),
                "guest playback mixer initialization failed; see audio-mixer.stderr"
            );
        }
        if !native {
            for (tag, directory) in [
                ("proton-home", "home"),
                ("proton-compatdata", "compatdata"),
                ("proton-cache", "cache"),
            ] {
                let shared = Path::new("/run/legion-shares").join(tag);
                if shared.is_dir() {
                    bind(
                        shared.to_str().context("non-UTF8 guest state path")?,
                        Path::new("/frames")
                            .join(directory)
                            .to_str()
                            .context("non-UTF8 guest state target")?,
                    )?;
                }
            }
        }
        let lock = Path::new("/frames/dist.lock");
        if !native && Path::new("/run/legion-shares/proton-tool").is_dir() {
            fs::write(
                "/frames/shared-proton-state.json",
                serde_json::to_vec(&serde_json::json!({
                    "proton_path":launch.proton_path,
                    "prefix_existed_before_setup":Path::new("/frames/compatdata/pfx/system.reg").is_file(),
                    "compatdata_shared":Path::new("/run/legion-shares/proton-compatdata").is_dir(),
                    "cache_shared":Path::new("/run/legion-shares/proton-cache").is_dir(),
                    "home_shared":Path::new("/run/legion-shares/proton-home").is_dir()
                }))?,
            )?;
        }
        if !lock.exists() {
            fs::write(lock, [])?;
        }
        if !native {
            chown(lock, launch.uid, launch.gid)?;
            bind(
                "/frames/dist.lock",
                &format!("{}/dist.lock", launch.proton_path),
            )?;
        }
        let wine_overlay = launch
            .wine_overlay
            .as_ref()
            .map(|overlay| {
                overlay.validate(
                    Path::new(&launch.proton_path),
                    Path::new("/usr/share/legion/compatibility/ntdll.dll"),
                )
            })
            .transpose()?;
        if let Some(overlay) = &wine_overlay {
            if !launch.ephemeral_wine_overlay {
                bind(
                    overlay
                        .replacement()
                        .to_str()
                        .context("non-UTF8 Wine overlay path")?,
                    &format!(
                        "{}/files/lib/wine/x86_64-windows/ntdll.dll",
                        launch.proton_path
                    ),
                )?;
            }
            let config = launch
                .wine_overlay
                .as_ref()
                .context("missing Wine overlay configuration")?;
            fs::write(
                "/frames/wine-overlay.json",
                serde_json::to_vec_pretty(&serde_json::json!({
                    "schema":1,"stock_pe_sha256":config.stock_pe_sha256,
                    "stock_unix_sha256":config.stock_unix_sha256,
                    "replacement_sha256":config.replacement_sha256,"migrate_loader":config.migrate_loader,
                    "scope":"private guest namespace; selected installed files stay readonly"
                }))?,
            )?;
        }
        if launch.external_benchmark {
            let mut command = Command::new("/usr/bin/python3");
            command
                .arg("/usr/share/legion/external_proton_bootstrap.py")
                .env("PYTHONDONTWRITEBYTECODE", "1");
            if native {
                command.arg("--native");
            }
            let status = command
                .status()
                .context("start external benchmark bootstrap")?;
            ensure!(
                status.success(),
                "external benchmark bootstrap failed: {status}"
            );
            return Ok(());
        }
        let application_executable = launch
            .application
            .as_ref()
            .map(|application| {
                application.resolve(
                    launch
                        .host_paths
                        .as_ref()
                        .map_or(Path::new("/run/legion-shares/application"), |paths| {
                            Path::new(&paths.application)
                        }),
                )
            })
            .transpose()?;
        let config = fs::read("/usr/share/legion/config.json")?;
        let parsed: legion_proton_workload::Config = serde_json::from_slice(&config)?;
        ensure!(parsed.valid(), "invalid Windows workload configuration");
        if launch.cpu_protocol_audit {
            match fs::remove_file("/frames/cpu-protocol-audit.json") {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("remove stale CPU/protocol audit"),
            }
        }
        let windowed = parsed.presentation.is_some();
        if launch.mangohud {
            ensure!(
                windowed && !native,
                "guest MangoHud requires a windowed guest workload"
            );
            ensure!(
                launch.mangohud_config.len() <= 4096 && !launch.mangohud_config.contains('\0'),
                "invalid MangoHud configuration"
            );
            ensure!(
                Path::new("/usr/share/vulkan/implicit_layer.d/MangoHud.x86_64.json").is_file()
                    && Path::new("/usr/lib/x86_64-linux-gnu/mangohud/libMangoHud.so").is_file(),
                "guest MangoHud runtime is missing; stage the guest-compatible overlay first"
            );
            fs::create_dir_all("/frames/mangohud")?;
            chown(Path::new("/frames/mangohud"), launch.uid, launch.gid)?;
        }
        ensure!(
            launch.display_backend == DisplayBackend::Auto || (windowed && !native),
            "explicit display backend requires a windowed guest workload"
        );
        let use_xwayland = if windowed
            && !native
            && Path::new("/run/legion-shares/proton-tool").is_dir()
        {
            let directory = Path::new(&launch.proton_path).join("files/lib/wine/x86_64-unix");
            let native_wayland = directory.read_dir().ok().is_some_and(|mut entries| {
                entries.any(|entry| {
                    entry.is_ok_and(|e| e.file_name().to_string_lossy().starts_with("winewayland"))
                })
            });
            match launch.display_backend {
                DisplayBackend::Auto => !native_wayland,
                DisplayBackend::Xwayland => true,
                DisplayBackend::Wayland => {
                    ensure!(
                        native_wayland,
                        "selected Proton has no native Wayland driver"
                    );
                    false
                }
            }
        } else {
            false
        };
        ensure!(
            !use_xwayland || Path::new("/usr/bin/Xwayland").is_file(),
            "selected Proton needs XWayland, but the guest runtime has no Xwayland executable"
        );
        if use_xwayland {
            // This path is in the disposable guest, after the PID1 check.
            fs::create_dir_all("/tmp/.X11-unix")?;
            fs::set_permissions("/tmp/.X11-unix", fs::Permissions::from_mode(0o1777))?;
        }
        ensure!(
            !launch.native_forward || windowed,
            "native forwarding requires the owned windowed fixture"
        );
        let direct_desktop = native && windowed && !launch.native_forward;
        if windowed {
            for name in [
                "proton-client-exited",
                "proton-forward-result.json",
                "xwayland-ready.json",
            ] {
                match fs::remove_file(Path::new("/frames").join(name)) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(error).context("remove stale owned presentation marker");
                    }
                }
            }
        }
        if !native && parsed.diagnostic_event_trace {
            enable_guest_trace()?;
        }
        if launch.synchronization_trace {
            ensure!(
                !native && windowed,
                "synchronization tracing requires the guest window fixture"
            );
            enable_guest_trace()?;
            for event in [
                "syscalls/sys_enter_membarrier",
                "ipi/ipi_send_cpu",
                "ipi/ipi_send_cpumask",
            ] {
                fs::write(format!("/sys/kernel/tracing/events/{event}/enable"), "1")?;
            }
        }
        fs::write("/frames/fixture-config.json", config)?;
        match fs::remove_file("/frames/workload-result.json") {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("remove previous owned workload result"),
        }
        if !native {
            chown(
                Path::new("/frames/fixture-config.json"),
                launch.uid,
                launch.gid,
            )?;
        }
        let runtime = format!("/run/user/{}", launch.uid);
        let server_log = fs::File::create("/frames/wayland.log")?;
        let mut server = Command::new("/usr/share/legion/compositor");
        server
            .arg(if windowed {
                if native {
                    "--proton-native-forward"
                } else {
                    "--proton-forward"
                }
            } else {
                "--offscreen-server"
            })
            .env_clear()
            .env("PATH", "/usr/bin")
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("LD_LIBRARY_PATH", "/usr/lib:/usr/lib/x86_64-linux-gnu")
            .env("__EGL_VENDOR_LIBRARY_DIRS", "/usr/share/glvnd/egl_vendor.d")
            .env("VK_DRIVER_FILES", launch.vulkan_driver.icd())
            .stdin(Stdio::null())
            .stdout(server_log.try_clone()?)
            .stderr(server_log);
        if !native {
            server.uid(launch.uid).gid(launch.gid);
        }
        if native && launch.native_forward {
            server.env("WAYLAND_DISPLAY", "legion-host-0");
        }
        let mut server = if direct_desktop {
            ensure!(
                Path::new(&runtime).join("legion-host-0").exists(),
                "native presentation requires the explicitly bound desktop socket"
            );
            None
        } else {
            if let Some(presentation) = &parsed.presentation {
                fs::write(
                    "/frames/proton-forward-config.json",
                    serde_json::to_vec(&serde_json::json!({
                        "schema":1,"external_client":true,"width":parsed.width,"height":parsed.height,
                        "seconds":presentation.seconds,"uncapped":true,"acquire_mode":"async",
                        "frame_transport":"polled","frame_pacing":"callbacks",
                        "forward_wait":launch.forward_wait.as_str(),
                        "explicit_sync":launch.explicit_sync,
                        "forward_idle_retry_us":launch.forward_idle_retry_us,
                        "xwayland":use_xwayland,"application_client":launch.application.is_some()
                        ,"diagnostic_frames":launch.thread_snapshots,
                        "live_telemetry":launch.presentation_telemetry
                    }))?,
                )?;
            }
            Some(OwnedServer(
                server.spawn().context("start owned Wayland services")?,
            ))
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while server.is_some() && !Path::new(&runtime).join("legion-proton-0").exists() {
            ensure!(
                server
                    .as_mut()
                    .context("missing owned server")?
                    .0
                    .try_wait()?
                    .is_none(),
                "Wayland service exited: {}",
                fs::read_to_string("/frames/wayland.log")?
            );
            ensure!(
                Instant::now() < deadline,
                "Wayland service did not become ready"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let x11_display = if use_xwayland {
            let ready = Path::new("/frames/xwayland-ready.json");
            let deadline = Instant::now() + Duration::from_secs(10);
            while !ready.is_file() {
                ensure!(
                    server
                        .as_mut()
                        .context("missing XWayland owner")?
                        .0
                        .try_wait()?
                        .is_none(),
                    "XWayland owner exited; inspect wayland.log and xwayland.log"
                );
                ensure!(
                    Instant::now() < deadline,
                    "XWayland did not become ready; inspect xwayland.log"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            let ready: serde_json::Value = serde_json::from_slice(&fs::read(ready)?)?;
            Some(
                ready["display"]
                    .as_str()
                    .context("missing XWayland display")?
                    .to_owned(),
            )
        } else {
            None
        };
        let _audio_snapshots = if launch.desktop_audio && launch.audio_trace && !native {
            Some(super::audio_diagnostics::AudioSnapshots::start()?)
        } else {
            None
        };
        let mut phases = Vec::new();
        if launch.cpu_adapter {
            ensure!(
                Path::new("/dev/legion_cpuid").exists(),
                "guest CPUID module is unavailable"
            );
            ensure!(
                Path::new(crate::cpu_adapter_control::LIBRARY).is_file(),
                "guest CPUID adapter is unavailable"
            );
            crate::cpu_adapter_control::create_state(Path::new("/frames/cpu-adapter-state.bin"))?;
            chown(
                Path::new("/frames/cpu-adapter-state.bin"),
                launch.uid,
                launch.gid,
            )?;

            let fixture = "/usr/share/legion/cpu-adapter-fixture";
            ensure!(
                Path::new(fixture).is_file(),
                "guest CPUID contract fixture is unavailable"
            );
            let mut fixture_command = Command::new(fixture);
            fixture_command.env_clear();
            if launch.nt_syscall_audit {
                fixture_command.env("LEGION_NT_SYSCALL", "1");
            }
            let status = fixture_command
                .env("LD_LIBRARY_PATH", "/usr/lib:/usr/lib/x86_64-linux-gnu")
                .env("LD_PRELOAD", crate::cpu_adapter_control::LIBRARY)
                .env("LEGION_CPU_ADAPTER", "1")
                .uid(launch.uid)
                .gid(launch.gid)
                .stdout(fs::File::create("/frames/cpu-adapter-fixture.stdout")?)
                .stderr(fs::File::create("/frames/cpu-adapter-fixture.stderr")?)
                .status()
                .context("run guest CPUID contract fixture")?;
            let counters =
                crate::cpu_adapter_control::read_state(Path::new("/frames/cpu-adapter-state.bin"))?;
            fs::write(
                "/frames/cpu-adapter-fixture-report.json",
                serde_json::to_vec_pretty(&counters)?,
            )?;
            ensure!(
                status.success(),
                "guest CPUID contract fixture failed: {status}"
            );
            fs::rename(
                "/frames/cpu-adapter-state.bin",
                "/frames/cpu-adapter-fixture-state.bin",
            )?;
            crate::cpu_adapter_control::create_state(Path::new("/frames/cpu-adapter-state.bin"))?;
            chown(
                Path::new("/frames/cpu-adapter-state.bin"),
                launch.uid,
                launch.gid,
            )?;
        }
        if launch.kuser_worker {
            let status = Command::new("/usr/share/legion/cpu-adapter-fixture")
                .arg("--zero-pid")
                .env_clear()
                .env("LD_LIBRARY_PATH", "/usr/lib:/usr/lib/x86_64-linux-gnu")
                .env("LD_PRELOAD", crate::cpu_adapter_control::LIBRARY)
                .env("LEGION_CPU_ADAPTER", "1")
                .env("LEGION_KUSER_WORKER", "1")
                .uid(launch.uid)
                .gid(launch.gid)
                .stdout(fs::File::create("/frames/kuser-zero-pid.stdout")?)
                .stderr(fs::File::create("/frames/kuser-zero-pid.stderr")?)
                .status()
                .context("run zero-PID KUSER control")?;
            let report =
                crate::cpu_adapter_control::read_state(Path::new("/frames/cpu-adapter-state.bin"))?;
            fs::write(
                "/frames/kuser-zero-pid.json",
                serde_json::to_vec_pretty(&report)?,
            )?;
            ensure!(status.success(), "zero-PID KUSER control failed: {status}");
            fs::rename(
                "/frames/cpu-adapter-state.bin",
                "/frames/kuser-zero-pid-state.bin",
            )?;
            crate::cpu_adapter_control::create_state(Path::new("/frames/cpu-adapter-state.bin"))?;
            chown(
                Path::new("/frames/cpu-adapter-state.bin"),
                launch.uid,
                launch.gid,
            )?;
        }
        let nvidia = if launch.nvidia_features {
            crate::nvidia_runtime::NvidiaRuntime::discover(
                Path::new("/usr"),
                Path::new(&launch.proton_path),
            )?
        } else {
            None
        };
        if launch.desktop_audio && launch.audio_diagnostics {
            let mut probes = Vec::new();
            for name in ["audio-probe", "audio-probe-concurrent"] {
                let child = Command::new("/usr/share/legion/audio-playback-probe")
                    .env_clear()
                    .env("LD_LIBRARY_PATH", "/usr/lib:/usr/lib/x86_64-linux-gnu")
                    .env("ALSA_CONFIG_PATH", "/tmp/legion-audio.conf")
                    .env("HOME", "/frames/home")
                    .uid(launch.uid)
                    .gid(launch.gid)
                    .stdin(Stdio::null())
                    .stdout(fs::File::create(format!("/frames/{name}.json"))?)
                    .stderr(fs::File::create(format!("/frames/{name}.stderr"))?)
                    .spawn()
                    .context("start owned playback probe")?;
                probes.push((name, OwnedServer(child)));
            }
            for (name, mut probe) in probes {
                let started = Instant::now();
                let status = loop {
                    if let Some(status) = probe.0.try_wait().context("poll owned playback probe")? {
                        break status;
                    }
                    ensure!(
                        started.elapsed() < Duration::from_secs(10),
                        "owned playback probe timed out; see {name}.stderr and audio-pcm.jsonl"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                };
                ensure!(
                    status.success(),
                    "owned playback probe failed: {status}; see {name}.stderr"
                );
            }
        }
        let mut jobs = vec![("prefix_setup", "getcompatpath", "workload.exe")];
        if launch.nvidia_diagnostics && nvidia.is_some() {
            ensure!(
                Path::new("/usr/share/legion/nvidia-probe.exe").is_file(),
                "missing owned NVIDIA diagnostic executable"
            );
            jobs.push(("nvidia_probe", "runinprefix", "nvidia-probe.exe"));
        }
        if launch.startup_contracts {
            jobs.extend([
                ("heap_contract", "runinprefix", "heap-list.exe"),
                ("loader_contract", "runinprefix", "loader-list.exe"),
                ("lifecycle_contract", "runinprefix", "loader-lifecycle.exe"),
            ]);
        }
        if launch.resolver_contracts {
            for phase in [
                "resolver_negative",
                "resolver_guarded",
                "resolver_frames",
                "resolver_shadow",
                "resolver_rwx",
                "resolver_unwind",
                "resolver_fault",
                "resolver_mismatch",
            ] {
                jobs.push((phase, "runinprefix", "resolver-contract.exe"));
            }
        }
        if launch.kuser_worker && launch.application.is_none() {
            jobs.push(("kuser_before", "runinprefix", "workload.exe"));
        }
        jobs.push(("workload", "runinprefix", "workload.exe"));
        if launch.kuser_worker && launch.application.is_none() {
            jobs.push(("kuser_after", "runinprefix", "workload.exe"));
        }
        for (phase, verb, executable) in jobs {
            let resolver_contract = phase.starts_with("resolver_");
            let adapter_phase = phase == "workload" || resolver_contract;
            println!("LEGION_PROTON_PHASE {phase}");
            if matches!(phase, "prefix_setup" | "workload") {
                fs::write(
                    "/frames/startup-status.json",
                    serde_json::to_vec(&serde_json::json!({
                        "schema":1,"stage":if phase=="prefix_setup" {"preparing-runtime"}else{"starting-application"}
                    }))?,
                )?;
            }
            let start = Instant::now();
            let mut command = Command::new("/usr/bin/python3");
            let executable = if phase == "workload" {
                application_executable
                    .clone()
                    .unwrap_or_else(|| Path::new("/usr/share/legion").join(executable))
            } else {
                Path::new("/usr/share/legion").join(executable)
            };
            command
                .arg(format!("{}/proton", launch.proton_path))
                .arg(verb)
                .arg(&executable)
                .current_dir("/frames")
                .env_clear()
                .env("PATH", "/usr/bin")
                .env("HOME", "/frames/home")
                .env("XDG_RUNTIME_DIR", format!("/run/user/{}", launch.uid))
                .env(
                    "WAYLAND_DISPLAY",
                    if direct_desktop {
                        "legion-host-0"
                    } else {
                        "legion-proton-0"
                    },
                )
                .env("PROTON_ENABLE_WAYLAND", "1")
                .env("XDG_CACHE_HOME", "/frames/cache")
                .env("PYTHONHOME", "/usr")
                .env("PYTHONDONTWRITEBYTECODE", "1")
                .env(
                    "LD_LIBRARY_PATH",
                    "/usr/lib:/usr/lib/x86_64-linux-gnu:/usr/lib/i386-linux-gnu",
                )
                .env("STEAM_COMPAT_DATA_PATH", "/frames/compatdata")
                .env("STEAM_COMPAT_CLIENT_INSTALL_PATH", "/frames/steam")
                .env("STEAM_COMPAT_INSTALL_PATH", "/usr/share/legion")
                .env("SteamAppId", "0")
                .env("SteamGameId", "0")
                // runinprefix skips setup_prefix's transient graphics overrides.
                .env("WINEDLLOVERRIDES", "dxgi,d3d12,d3d12core=n")
                .env("LANG", "C.UTF-8")
                .env(
                    "WINEDEBUG",
                    if launch.server_trace {
                        ensure!(windowed, "server tracing requires the owned window fixture");
                        "+timestamp,+pid,+tid,+server"
                    } else if launch.exception_trace {
                        "+timestamp,+pid,+tid,+seh,+msgbox"
                    } else if launch.cpu_adapter && launch.debug {
                        "+timestamp,+pid,+tid,+process,+seh,+loaddll,+msgbox"
                    } else if launch.debug {
                        "+timestamp,+pid,+loaddll,+waylanddrv,+vulkan"
                    } else {
                        "-all"
                    },
                )
                .env("DXVK_LOG_LEVEL", "info")
                .env("VKD3D_DEBUG", if launch.debug { "trace" } else { "warn" })
                .env("FONTCONFIG_FILE", "/usr/etc/fonts/fonts.conf")
                .env("FONTCONFIG_PATH", "/usr/etc/fonts")
                .env("VK_DRIVER_FILES", launch.vulkan_driver.icd())
                .env("VKD3D_SHADER_CACHE_PATH", "/frames/cache")
                .stdin(Stdio::null())
                .stdout(fs::File::create(format!("/frames/proton-{phase}.stdout"))?)
                .stderr(fs::File::create(format!("/frames/proton-{phase}.stderr"))?);
            if phase == "workload"
                && let Some(application) = &launch.application
            {
                command
                    .args(&application.arguments)
                    .current_dir(
                        executable
                            .parent()
                            .context("application directory missing")?,
                    )
                    .env(
                        "STEAM_COMPAT_INSTALL_PATH",
                        "/run/legion-shares/application",
                    )
                    .env("WINEDLLOVERRIDES", &application.dll_overrides);
                fs::write(
                    "/frames/application-launch.json",
                    serde_json::to_vec_pretty(&serde_json::json!({
                    "schema":1,"executable":executable.to_str().context("non-UTF8 application executable")?,"arguments":application.arguments,
                        "dll_overrides":application.dll_overrides,"cpu_adapter":launch.cpu_adapter,
                        "kuser_worker":launch.kuser_worker,"direct_nt":launch.nt_syscall_audit,
                        "wine_dispatch":launch.wine_dispatch_audit,"complete_profile_verified":false
                    }))?,
                )?;
            }
            if let Some(nvidia) = &nvidia {
                command
                    .env("NVIDIA_WINE_DLL_DIR", &nvidia.driver_directory)
                    .env("DXVK_ENABLE_NVAPI", "1");
                if phase == "workload" || phase == "nvidia_probe" {
                    let prepared = nvidia.prepare_prefix(Path::new("/frames/compatdata/pfx"))?;
                    for path in &prepared {
                        chown(path, launch.uid, launch.gid)?;
                    }
                    let base = if phase == "workload" {
                        launch
                            .application
                            .as_ref()
                            .map_or("dxgi,d3d12,d3d12core=n", |app| app.dll_overrides.as_str())
                    } else {
                        "dxgi,d3d12,d3d12core=n"
                    };
                    let overrides = crate::nvidia_runtime::overrides(base);
                    command.env("WINEDLLOVERRIDES", &overrides);
                    let prepared_names = prepared
                        .iter()
                        .map(|path| path.to_str().context("non-UTF8 private NVIDIA path"))
                        .collect::<Result<Vec<_>>>()?;
                    fs::write(
                        "/frames/nvidia-runtime.json",
                        serde_json::to_vec_pretty(&serde_json::json!({
                            "schema":1,"driver_directory":nvidia.driver_directory.to_str().context("non-UTF8 NVIDIA driver path")?,"private_prefix_files":prepared_names,
                            "dll_overrides":overrides,"feature_initialization_verified":false
                        }))?,
                    )?;
                    if launch.nvidia_diagnostics {
                        command
                            .env("DXVK_NVAPI_LOG_LEVEL", "info")
                            .env("DXVK_NVAPI_LOG_PATH", "/frames");
                    }
                }
            }
            if phase == "workload" && launch.mangohud {
                command
                    .env("MANGOHUD", "1")
                    .env("MANGOHUD_CONFIG", &launch.mangohud_config);
            }
            if launch.desktop_audio {
                command.env("ALSA_CONFIG_PATH", "/tmp/legion-audio.conf");
            }
            for (key, value) in &launch.environment {
                ensure!(
                    key.len() <= 128
                        && value.len() <= 16384
                        && !value.contains('\0')
                        && (key.starts_with("PROTON_")
                            || key.starts_with("DXVK_")
                            || key.starts_with("VKD3D_")
                            || matches!(
                                key.as_str(),
                                "SteamAppId"
                                    | "SteamGameId"
                                    | "WINEDEBUG"
                                    | "LANG"
                                    | "LC_ALL"
                                    | "LEGION_WORKLOAD_DIRECTORY"
                                    | "XDG_CACHE_HOME"
                                    | "XDG_CONFIG_HOME"
                                    | "XDG_DATA_HOME"
                                    | "XDG_STATE_HOME"
                            )),
                    "unsupported forwarded game environment variable"
                );
                command.env(key, value);
            }
            if launch.audio_trace && phase == "workload" {
                command.env(
                    "WINEDEBUG",
                    "+timestamp,+pid,+tid,+mmdevapi,+alsa,+xaudio2,+dsound",
                );
            }
            if resolver_contract {
                match phase {
                    "resolver_negative" => {}
                    "resolver_mismatch" => {
                        command.arg("--mismatched");
                    }
                    "resolver_frames" => {
                        command.args(["--guarded", "--lookup-frames"]);
                    }
                    "resolver_shadow" => {
                        command.args(["--guarded", "--shadow"]);
                    }
                    "resolver_rwx" => {
                        command.args(["--guarded", "--rwx"]);
                    }
                    "resolver_unwind" => {
                        command.args(["--guarded", "--unwind"]);
                    }
                    "resolver_fault" => {
                        command.args(["--guarded", "--fault"]);
                    }
                    _ => {
                        command.arg("--guarded");
                    }
                }
            }
            if let Some(display) = &x11_display {
                command
                    .env_remove("PROTON_ENABLE_WAYLAND")
                    .env("DISPLAY", display);
            }
            if launch
                .wine_overlay
                .as_ref()
                .is_some_and(|overlay| overlay.migrate_loader)
            {
                command.env("WINE_GATE_PRIVATE_LOADER", "2");
            }
            if phase == "loader_contract" {
                command.env("OWNED_GATE_EXPECT_LOADER_MIGRATION", "1");
            }
            if launch.cpu_protocol_audit && phase == "workload" {
                command.env("LEGION_CPU_PROTOCOL_AUDIT", "1");
            }
            if launch.cpu_adapter && adapter_phase {
                command
                    .env("LD_PRELOAD", crate::cpu_adapter_control::LIBRARY)
                    .env("LEGION_CPU_ADAPTER", "1");
            }
            if let Some(profile) = &launch.queue_latency_profile
                && phase == "workload"
            {
                ensure!(
                    windowed
                        && !native
                        && launch.application.is_none()
                        && launch.queue_placement.is_none()
                        && !launch.queue_cpu_profile
                        && !launch.queue_endpoint_profile
                        && !launch.syscall_counts,
                    "queue latency requires the owned guest fixture without other samplers"
                );
                let library = "/usr/share/legion/liblegion_queue_latency_probe.so";
                ensure!(
                    Path::new(library).is_file(),
                    "queue latency observer is missing"
                );
                super::cpu_profile::CpuProfile::prepare_queue_latency(
                    profile.opcode,
                    profile.stride,
                    launch.uid,
                    launch.gid,
                )?;
                let preload = if launch.cpu_adapter {
                    format!("{library}:{}", crate::cpu_adapter_control::LIBRARY)
                } else {
                    library.into()
                };
                command
                    .env("LD_PRELOAD", preload)
                    .env("LEGION_QUEUE_LATENCY_PATH", "/frames/queue-latency.bin");
            }
            if launch.thread_exit_trace && phase == "workload" {
                command.env("LEGION_EXIT_DIAGNOSTICS", "1");
            }
            if launch.kuser_worker && phase == "workload" {
                command.env("LEGION_KUSER_WORKER", "1");
            }
            if launch.nt_syscall_audit && phase == "workload" {
                command.env("LEGION_NT_SYSCALL", "1");
            }
            if launch.wine_dispatch_audit && phase == "workload" {
                command.env("LEGION_WINE_DISPATCH", "1");
            }
            if launch.resolver_bridge
                && (phase == "workload" || (resolver_contract && phase != "resolver_negative"))
            {
                command.env("LEGION_RESOLVER_BRIDGE", "1");
            }
            if phase == "kuser_before" {
                command.env("LEGION_KUSER_OBSERVER", "1");
            }
            if phase == "kuser_after" {
                command.env("LEGION_KUSER_OBSERVER", "2");
            }
            if launch.queue_profile && phase == "workload" {
                ensure!(
                    windowed,
                    "queue profile is restricted to the owned window fixture"
                );
                command.env("VKD3D_QUEUE_PROFILE", "Z:\\tmp\\vkd3d-queue-profile.json");
            }
            if launch.call_probe && phase == "workload" {
                ensure!(
                    windowed
                        && !launch.no_uvm_layer
                        && !launch.semaphore_probe
                        && !launch.attach_submit_fence,
                    "call observer requires the owned window fixture without other Vulkan diagnostic layers"
                );
                command
                    .env("VK_LAYER_PATH", "/usr/share/legion/call-probe")
                    .env("VK_INSTANCE_LAYERS", "VK_LAYER_NVGPU_no_uvm")
                    .env("LEGION_CALL_PROBE", "1");
            }
            if !native {
                command.uid(launch.uid).gid(launch.gid);
            }
            if !launch.vkd3d_config.is_empty() {
                command.env("VKD3D_CONFIG", &launch.vkd3d_config);
            }
            if let Some(value) = &launch.driver_yield {
                eprintln!("owned fixture driver yield: {}", value.as_str());
                command.env("__GL_YIELD", value.as_str());
            }
            if let Some(mode) = &launch.native_semsurf_control {
                ensure!(
                    native && windowed,
                    "semaphore-surface control requires the owned native window fixture"
                );
                if phase == "workload" {
                    command
                        .env("LD_PRELOAD", "/usr/share/legion/native-semsurf-control.so")
                        .env(
                            "LEGION_NATIVE_SEMSURF",
                            match mode {
                                NativeSemsurf::Pass => "pass",
                                NativeSemsurf::Disable => "disable",
                            },
                        );
                }
            }
            if launch.no_uvm_layer {
                command
                    .env("VK_LAYER_PATH", "/usr/share/vulkan/explicit_layer.d")
                    .env("VK_INSTANCE_LAYERS", "VK_LAYER_NVGPU_no_uvm");
            }
            if launch.semaphore_probe || launch.attach_submit_fence || launch.semaphore_spin_us > 0
            {
                command
                    .env("VK_LAYER_PATH", "/usr/share/vulkan/explicit_layer.d")
                    .env("VK_INSTANCE_LAYERS", "VK_LAYER_NVGPU_no_uvm")
                    .env("LEGION_SEMAPHORE_PROBE", "1")
                    .env("LEGION_SEMAPHORE_LOG", "/frames/semaphore-probe.log");
            }
            if launch.attach_submit_fence {
                command.env("LEGION_ATTACH_SUBMIT_FENCE", "1");
            }
            if launch.semaphore_memory_probe {
                ensure!(
                    launch.semaphore_probe || launch.attach_submit_fence,
                    "driver memory probe requires the explicit semaphore layer"
                );
                command.env("LEGION_SEMAPHORE_MEMORY_PROBE", "615.78.08");
            }
            if launch.semaphore_spin_us > 0 {
                ensure!(
                    launch.semaphore_spin_us <= 2000,
                    "semaphore spin budget exceeds diagnostic limit"
                );
                command.env(
                    "LEGION_SEMAPHORE_SPIN_US",
                    launch.semaphore_spin_us.to_string(),
                );
            }
            let snapshots = if launch.thread_snapshots && !native && phase == "workload" {
                Some(ThreadSnapshots::start(launch.thread_exit_trace)?)
            } else {
                None
            };
            let mut clock = if launch.gpu_clock_probe && phase == "workload" {
                let mut clock = Command::new("/usr/share/legion/gpu-clock-probe");
                clock
                    .args(["0", "0", "60000", "50"])
                    .env(
                        "LD_LIBRARY_PATH",
                        "/usr/lib:/usr/lib/x86_64-linux-gnu:/usr/lib/i386-linux-gnu",
                    )
                    .env("VK_DRIVER_FILES", launch.vulkan_driver.icd())
                    .stdin(Stdio::null())
                    .stdout(fs::File::create("/frames/gpu-clock.jsonl")?)
                    .stderr(fs::File::create("/frames/gpu-clock.stderr")?);
                if !native {
                    clock.uid(launch.uid).gid(launch.gid);
                }
                Some(OwnedServer(
                    clock.spawn().context("start owned GPU clock probe")?,
                ))
            } else {
                None
            };
            if let Some(mode) = &launch.guest_wakeups {
                ensure!(
                    windowed && !native,
                    "wakeup control requires the owned guest window fixture"
                );
                if phase == "workload" {
                    super::cpu_profile::CpuProfile::configure_guest_wakeups(matches!(
                        mode,
                        GuestWakeups::Direct
                    ))?;
                }
            }
            let profile = if launch.queue_latency_profile.is_some() && phase == "workload" {
                Some(super::cpu_profile::CpuProfile::start_queue_latency()?)
            } else if let Some(mode) = &launch.queue_placement {
                ensure!(
                    windowed && !native && !launch.syscall_counts && !launch.present_cpu_profile,
                    "queue placement requires the owned guest and call marker without sampling"
                );
                if phase == "workload" {
                    Some(super::cpu_profile::CpuProfile::start_queue_placement(
                        matches!(
                            mode,
                            QueuePlacement::Colocated | QueuePlacement::ServerColocated
                        ),
                        matches!(
                            mode,
                            QueuePlacement::ServerOriginal | QueuePlacement::ServerColocated
                        ),
                    )?)
                } else {
                    None
                }
            } else if launch.syscall_counts && phase == "workload" {
                ensure!(
                    windowed
                        && !native
                        && launch.application.is_none()
                        && !launch.present_cpu_profile,
                    "syscall counts require the owned VM fixture without CPU sampling"
                );
                Some(super::cpu_profile::CpuProfile::start_syscalls()?)
            } else if launch.queue_endpoint_profile && phase == "workload" {
                ensure!(
                    windowed && !native && launch.application.is_none(),
                    "endpoint profiling requires the owned guest fixture"
                );
                Some(super::cpu_profile::CpuProfile::start_queue_endpoints()?)
            } else if launch.queue_cpu_profile && phase == "workload" {
                ensure!(
                    windowed && !native && launch.application.is_none(),
                    "queue worker profiling requires the owned guest fixture"
                );
                Some(super::cpu_profile::CpuProfile::start_queue_worker(
                    launch.queue_cycle_profile,
                    launch.queue_profile_include_kernel,
                )?)
            } else if launch.present_cpu_profile && phase == "workload" {
                ensure!(
                    windowed && launch.call_probe,
                    "present worker profiling requires the explicit owned call observer"
                );
                Some(super::cpu_profile::CpuProfile::start_present_worker()?)
            } else if parsed.diagnostic_cpu_profile
                && !launch.cpu_profile_markers_only
                && phase == "workload"
            {
                Some(super::cpu_profile::CpuProfile::start()?)
            } else {
                None
            };
            if windowed && !native && phase == "workload" {
                fs::write(
                    "/frames/interrupts-before.txt",
                    fs::read("/proc/interrupts")?,
                )?;
            }
            if windowed && phase == "workload" {
                fs::write(
                    "/frames/clock-before.json",
                    serde_json::to_vec_pretty(&clock_environment())?,
                )?;
            }
            let mut child = command
                .spawn()
                .with_context(|| format!("start Proton {phase}"))?;
            let status = child
                .wait()
                .with_context(|| format!("run Proton {phase}"))?;
            if launch.synchronization_trace && phase == "workload" {
                fs::write("/sys/kernel/tracing/tracing_on", "0")?;
                fs::write(
                    "/frames/synchronization-guest.trace",
                    fs::read("/sys/kernel/tracing/trace")?,
                )?;
            }
            if windowed && !native && phase == "workload" {
                fs::write(
                    "/frames/interrupts-after.txt",
                    fs::read("/proc/interrupts")?,
                )?;
            }
            if windowed && phase == "workload" {
                fs::write(
                    "/frames/clock-after.json",
                    serde_json::to_vec_pretty(&clock_environment())?,
                )?;
            }
            if let Some(profile) = profile {
                profile.finish()?;
            }
            drop(snapshots);
            if let Some(clock) = &mut clock {
                if let Some(status) = clock.0.try_wait()? {
                    ensure!(
                        status.success(),
                        "GPU clock probe failed: {status}; see gpu-clock.stderr"
                    );
                }
                ensure!(
                    fs::metadata("/frames/gpu-clock.jsonl")?.len() > 0,
                    "GPU clock probe produced no samples"
                );
            }
            drop(clock);
            if !native && parsed.diagnostic_event_trace && phase == "workload" {
                fs::write("/sys/kernel/tracing/tracing_on", "0")?;
                fs::write(
                    "/frames/event-timeline-guest.trace",
                    fs::read("/sys/kernel/tracing/trace")?,
                )?;
            }
            phases.push(serde_json::json!({"phase":phase,"elapsed_ms":start.elapsed().as_millis(),"success":status.success()}));
            fs::write(
                "/frames/proton-phases.json",
                serde_json::to_vec_pretty(&phases)?,
            )?;
            if launch.cpu_adapter && phase == "workload" {
                let counters = crate::cpu_adapter_control::read_state(Path::new(
                    "/frames/cpu-adapter-state.bin",
                ))?;
                fs::write(
                    "/frames/cpu-adapter-report.json",
                    serde_json::to_vec_pretty(&counters)?,
                )?;
            }
            if resolver_contract {
                let path = Path::new("/frames/cpu-adapter-state.bin");
                let counters = crate::cpu_adapter_control::read_state(path)?;
                fs::write(
                    format!("/frames/{phase}-state.json"),
                    serde_json::to_vec_pretty(&counters)?,
                )?;
                fs::rename(path, format!("/frames/{phase}-state.bin"))?;
                crate::cpu_adapter_control::create_state(path)?;
                chown(path, launch.uid, launch.gid)?;
            }
            ensure!(
                status.success(),
                "Proton {phase} failed: {status}; see /frames/proton-{phase}.stdout and .stderr"
            );
            if phase == "prefix_setup"
                && let Some(overlay) = &wine_overlay
            {
                let module = Path::new("/frames/compatdata/pfx/drive_c/windows/system32/ntdll.dll");
                let changed = if launch.ephemeral_wine_overlay {
                    overlay.validate_prefix(module)?;
                    bind(
                        overlay
                            .replacement()
                            .to_str()
                            .context("invalid overlay path")?,
                        module.to_str().context("invalid prefix path")?,
                    )?;
                    bind(
                        overlay
                            .replacement()
                            .to_str()
                            .context("invalid overlay path")?,
                        &format!(
                            "{}/files/lib/wine/x86_64-windows/ntdll.dll",
                            launch.proton_path
                        ),
                    )?;
                    false
                } else {
                    overlay.prepare_prefix(module)?
                };
                if changed {
                    chown(module, launch.uid, launch.gid)?;
                }
                fs::write(
                    "/frames/wine-overlay-prefix.json",
                    serde_json::to_vec(&serde_json::json!({
                        "schema":1,"prefix_updated":changed,"replacement_verified":true
                    }))?,
                )?;
            }
            if launch.cpu_protocol_audit && phase == "workload" {
                let path = Path::new("/frames/cpu-protocol-audit.json");
                ensure!(
                    (1..=4 * 1024 * 1024).contains(
                        &fs::metadata(path)
                            .context("owned executable did not produce its CPU/protocol audit")?
                            .len()
                    ),
                    "invalid CPU/protocol audit size"
                );
                let capture: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
                ensure!(
                    capture["schema"] == 1
                        && capture["capture_complete"] == true
                        && capture["queries"]
                            .as_u64()
                            .is_some_and(|count| (1..=65536).contains(&count)),
                    "owned CPU/protocol audit is incomplete"
                );
            }
            if launch.queue_profile && phase == "workload" {
                let trace = Path::new("/tmp/vkd3d-queue-profile.json");
                ensure!(
                    (1..=512 * 1024 * 1024).contains(&fs::metadata(trace)?.len()),
                    "queue timeline exceeds bounded diagnostic size"
                );
                fs::copy(trace, "/frames/vkd3d-queue-profile.json")?;
            }
            if launch.call_probe && phase == "workload" {
                let path = Path::new("/tmp/legion-vk-calls.json");
                ensure!(
                    (1..=65536).contains(&fs::metadata(path)?.len()),
                    "invalid call observer report size"
                );
                let report: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
                ensure!(
                    report["schema"] == 1
                        && report["calls"]["vkQueuePresentKHR"]["samples"]
                            .as_u64()
                            .unwrap_or(0)
                            > 0,
                    "Vulkan call observer did not sample presentation"
                );
                fs::copy(path, "/frames/vulkan-call-profile.json")?;
            }
        }
        if windowed {
            if launch.application.is_none() {
                let report: serde_json::Value =
                    serde_json::from_slice(&fs::read("/frames/workload-result.json")?)?;
                ensure!(
                    report["passed"] == true
                        && report["gpu_verified"] == true
                        && report["original_queue_drained"] == true,
                    "presentation GPU validation failed"
                );
            } else {
                fs::write(
                    "/frames/workload-result.json",
                    serde_json::to_vec(&serde_json::json!({
                        "schema":1,"passed":true,"gpu_verified":false,
                        "workload":"application_exit","complete_profile_verified":false
                    }))?,
                )?;
            }
            if let Some(server) = server.as_mut() {
                fs::write("/frames/proton-client-exited", [])?;
                let deadline = Instant::now() + Duration::from_secs(10);
                let status = loop {
                    if let Some(status) = server.0.try_wait()? {
                        break status;
                    }
                    ensure!(
                        Instant::now() < deadline,
                        "presentation bridge did not drain"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                };
                ensure!(
                    status.success(),
                    "presentation bridge failed; inspect wayland.log"
                );
                let forward: serde_json::Value =
                    serde_json::from_slice(&fs::read("/frames/proton-forward-result.json")?)?;
                ensure!(
                    forward["passed"] == true && forward["full_frame_cpu_transport"] == false,
                    "presentation bridge did not verify GPU forwarding"
                );
            }
        } else {
            let report: legion_proton_workload::Report =
                serde_json::from_slice(&fs::read("/frames/workload-result.json")?)?;
            ensure!(
                report.passed && report.gpu_verified,
                "Windows workload did not verify GPU output"
            );
        }
        Ok(())
    }
}

fn main() {
    #[cfg(unix)]
    if let Err(error) = linux::run() {
        eprintln!("Proton bootstrap failed: {error:#}");
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    compile_error!("the bootstrap must be built for Linux");
}
