use std::{
    collections::{HashMap, VecDeque},
    io::{self, BufRead, BufReader},
    os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    ptr,
    sync::{Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows_sys::Win32::System::{
    Diagnostics::Etw::{
        ControlTraceW, CONTROLTRACE_HANDLE, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_PROPERTIES,
        WNODE_FLAG_TRACED_GUID,
    },
    JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
};

const MAX_CHAINS: usize = 8;
const MAX_FRAMES: usize = 32_768;
const FRESH: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug)]
pub struct FrameSample {
    pub status: &'static str,
    pub fps: Option<f64>,
    pub low_fps: Option<f64>,
    pub frame_ms: Option<f64>,
}

impl FrameSample {
    fn empty(status: &'static str) -> Self {
        Self {
            status,
            fps: None,
            low_fps: None,
            frame_ms: None,
        }
    }
}

#[derive(Clone, Copy)]
struct Interval {
    end: f64, // ETW event timestamp in seconds, independent of pipe/reader scheduling.
    ms: f64,
}

struct Chain {
    frames: VecDeque<Interval>,
    received: Instant,
}

#[derive(Default)]
struct FrameHistory {
    chains: HashMap<u64, Chain>,
    primary: Option<u64>,
}

impl FrameHistory {
    fn push(&mut self, address: u64, frame: Interval, now: Instant) {
        if self.chains.len() == MAX_CHAINS && !self.chains.contains_key(&address) {
            if let Some((&oldest, _)) = self.chains.iter().min_by_key(|(_, chain)| chain.received) {
                self.chains.remove(&oldest);
            }
        }
        let chain = self.chains.entry(address).or_insert_with(|| Chain {
            frames: VecDeque::new(),
            received: now,
        });
        if chain
            .frames
            .back()
            .is_some_and(|prev| frame.end <= prev.end)
        {
            return;
        }
        while chain.frames.len() >= MAX_FRAMES
            || chain
                .frames
                .front()
                .is_some_and(|prev| frame.end - prev.end >= 10.0)
        {
            chain.frames.pop_front();
        }
        chain.frames.push_back(frame);
        chain.received = now;
    }

    fn sample(&mut self, now: Instant) -> FrameSample {
        self.chains
            .retain(|_, chain| now.duration_since(chain.received) < FRESH);
        let newest = self
            .chains
            .values()
            .filter_map(|c| c.frames.back())
            .map(|f| f.end)
            .fold(0.0, f64::max);
        let rate = |chain: &Chain| {
            chain
                .frames
                .iter()
                .rev()
                .take_while(|f| newest - f.end < 1.0)
                .count()
        };
        let candidate = self.chains.iter().max_by_key(|(_, chain)| rate(chain));
        let Some((&winner, best)) = candidate else {
            return FrameSample::empty("collecting");
        };
        // Keep the dominant chain stable when splash/UI chains present at a similar cadence.
        // Never combine timestamps from different swap chains into a synthetic frame rate.
        let previous = self.primary.and_then(|id| self.chains.get(&id));
        if previous.is_none_or(|c| rate(c) * 4 < rate(best) * 3) {
            self.primary = Some(winner);
        }
        let chain = &self.chains[&self.primary.unwrap_or(winner)];
        let recent = rate(chain);
        if recent == 0 {
            return FrameSample::empty("collecting");
        }
        let mean = chain
            .frames
            .iter()
            .rev()
            .take(recent)
            .map(|f| f.ms)
            .sum::<f64>()
            / recent as f64;
        let low_fps = if chain.frames.len() >= 100 {
            let mut intervals: Vec<_> = chain.frames.iter().map(|f| f.ms).collect();
            let n = intervals.len().div_ceil(100);
            intervals.select_nth_unstable_by(n - 1, |a, b| b.total_cmp(a));
            Some(1000.0 * n as f64 / intervals[..n].iter().sum::<f64>())
        } else {
            None
        };
        FrameSample {
            status: "live",
            fps: Some(1000.0 / mean),
            low_fps,
            frame_ms: Some(mean),
        }
    }
}

// PresentMon writes process names without quoting them. Locate columns from the right
// so a comma or a non-UTF8 character in an executable name cannot shift the values.
struct Columns {
    pid: usize,
    chain: usize,
    time: usize,
    interval: usize,
}

impl Columns {
    fn parse(header: &[u8]) -> Option<Self> {
        let find = |name: &[u8]| {
            header
                .rsplit(|&b| b == b',')
                .position(|s| s.eq_ignore_ascii_case(name))
        };
        Some(Self {
            pid: find(b"ProcessID")?,
            chain: find(b"SwapChainAddress")?,
            time: find(b"TimeInSeconds")?,
            interval: find(b"MsBetweenPresents")?,
        })
    }

    fn frame(&self, row: &[u8], pid: u32) -> Option<(u64, Interval)> {
        let field = |i| std::str::from_utf8(row.rsplit(|&b| b == b',').nth(i)?).ok();
        if field(self.pid)?.parse::<u32>().ok()? != pid {
            return None;
        }
        let address = u64::from_str_radix(field(self.chain)?.strip_prefix("0x")?, 16).ok()?;
        let end: f64 = field(self.time)?.parse().ok()?;
        let ms: f64 = field(self.interval)?.parse().ok()?;
        (end.is_finite() && end >= 0.0 && ms.is_finite() && ms > 0.0)
            .then_some((address, Interval { end, ms }))
    }
}

fn collect(reader: impl BufRead, pid: u32, history: Arc<Mutex<FrameHistory>>) {
    let mut reader = reader;
    let mut columns = None;
    let mut row = Vec::with_capacity(384);
    loop {
        row.clear();
        if !matches!(reader.read_until(b'\n', &mut row), Ok(1..)) {
            break;
        }
        let row = row.trim_ascii_end();
        if let Some((chain, frame)) = columns.as_ref().and_then(|c: &Columns| c.frame(row, pid)) {
            history.lock().unwrap().push(chain, frame, Instant::now());
        } else if row.starts_with(b"Application,") {
            columns = Columns::parse(row);
        }
    }
}

struct Session {
    child: Child,
    _job: OwnedHandle,
    history: Arc<Mutex<FrameHistory>>,
    errors: Option<JoinHandle<bool>>,
    name: String,
}

fn job_for(child: &Child) -> io::Result<OwnedHandle> {
    // The kernel closes this non-inherited handle even if the monitor crashes.
    unsafe {
        let handle = CreateJobObjectW(ptr::null(), ptr::null());
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = OwnedHandle::from_raw_handle(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            handle,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size_of_val(&limits) as u32,
        ) == 0
            || AssignProcessToJobObject(handle, child.as_raw_handle()) == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }
}

fn stop_trace(name: &str) {
    // TerminateProcess alone can leave ETW's kernel-side recording alive.
    #[repr(C)]
    struct Properties {
        trace: EVENT_TRACE_PROPERTIES,
        logger: [u16; 1024],
        logfile: [u16; 1024],
    }
    let mut properties = Properties {
        trace: EVENT_TRACE_PROPERTIES::default(),
        logger: [0; 1024],
        logfile: [0; 1024],
    };
    properties.trace.Wnode.BufferSize = size_of_val(&properties) as u32;
    properties.trace.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
    properties.trace.LoggerNameOffset = std::mem::offset_of!(Properties, logger) as u32;
    properties.trace.LogFileNameOffset = std::mem::offset_of!(Properties, logfile) as u32;
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    unsafe {
        ControlTraceW(
            CONTROLTRACE_HANDLE::default(),
            name.as_ptr(),
            &mut properties.trace,
            EVENT_TRACE_CONTROL_STOP,
        );
    }
}

impl Session {
    fn start(exe: &Path, pid: u32) -> io::Result<Self> {
        let name = format!("SystemBioMon-Frames-{}-{pid}", std::process::id());
        let mut child = Command::new(exe)
            .args([
                "--process_id",
                &pid.to_string(),
                "--session_name",
                &name,
                "--v1_metrics",
                "--output_stdout",
                "--no_console_stats",
                "--no_track_display",
                "--no_track_gpu",
                "--no_track_input",
                "--terminate_on_proc_exit",
            ])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let job = match job_for(&child) {
            Ok(job) => job,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                stop_trace(&name);
                return Err(err);
            }
        };
        let history = Arc::<Mutex<FrameHistory>>::default();
        let reader_history = history.clone();
        let out = child.stdout.take().unwrap();
        thread::spawn(move || collect(BufReader::new(out), pid, reader_history));
        let errors = child.stderr.take().unwrap();
        let errors = thread::spawn(move || {
            let mut denied = false;
            for line in BufReader::new(errors).lines().map_while(Result::ok) {
                denied |= line.to_ascii_lowercase().contains("access denied");
            }
            denied
        });
        Ok(Self {
            child,
            _job: job,
            history,
            errors: Some(errors),
            name,
        })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        stop_trace(&self.name);
    }
}

pub struct FrameCapture {
    helper: PathBuf,
    target: Option<u32>,
    session: Option<Session>,
    failure: Option<(&'static str, Instant)>,
}

impl FrameCapture {
    pub fn new(resource_dir: &Path) -> Self {
        Self {
            helper: resource_dir.join("resources/presentmon/PresentMon.exe"),
            target: None,
            session: None,
            failure: None,
        }
    }

    pub fn stop(&mut self) {
        self.session = None;
        self.target = None;
    }

    pub fn sample(&mut self, pid: Option<u32>) -> FrameSample {
        if self.target != pid {
            self.stop();
            self.target = pid;
            if self
                .failure
                .is_some_and(|(reason, _)| reason != "permission")
            {
                self.failure = None;
            }
        }
        let Some(pid) = pid else {
            return FrameSample::empty("idle");
        };
        let now = Instant::now();
        if let Some((reason, time)) = self.failure {
            if now.duration_since(time) < Duration::from_secs(30) {
                return FrameSample::empty(reason);
            }
            self.failure = None;
        }
        if self.session.is_none() {
            match Session::start(&self.helper, pid) {
                Ok(session) => self.session = Some(session),
                Err(_) => {
                    self.failure = Some(("unavailable", now));
                    return FrameSample::empty("unavailable");
                }
            }
        }
        let session = self.session.as_mut().unwrap();
        match session.child.try_wait() {
            Ok(None) => session.history.lock().unwrap().sample(now),
            ended => {
                let denied = ended.is_ok()
                    && session
                        .errors
                        .take()
                        .is_some_and(|t| t.join().unwrap_or(false));
                let reason = if denied { "permission" } else { "unavailable" };
                self.session = None;
                self.failure = Some((reason, now));
                FrameSample::empty(reason)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pinned_csv_and_ignores_other_pids_and_missing_intervals() {
        let csv = b"Application,ProcessID,SwapChainAddress,Runtime,SyncInterval,PresentFlags,Dropped,TimeInSeconds,msInPresentAPI,msBetweenPresents,AllowsTearing,PresentMode\r\n\
demo,app.exe,42,0xA,DXGI,1,0,0,0.5,0,20,1,Composed: Flip\r\n\
demo.exe,9,0xA,DXGI,1,0,0,0.6,0,150,1,Composed: Flip\r\n\
demo.exe,42,0xB,DXGI,1,0,0,0.8,0,NA,1,Composed: Flip\r\n\
demo.exe,42,0xB,DXGI,1,0,0,0.9,0,0,1,Composed: Flip\r\n\
demo.exe,42,0xA,DXGI,1,0,0,1,0,inf,1,Composed: Flip\r\n";
        let history = Arc::<Mutex<FrameHistory>>::default();
        collect(&csv[..], 42, history.clone());
        let mut history = history.lock().unwrap();
        assert_eq!(history.chains.len(), 1);
        assert_eq!(history.chains[&10].frames.len(), 1);
        let sample = history.sample(Instant::now());
        assert_eq!(sample.fps, Some(50.0));
        assert_eq!(sample.frame_ms, Some(20.0));
        assert_eq!(sample.low_fps, None);
    }

    #[test]
    fn separates_chains_expires_stale_values_and_exposes_a_one_percent_low() {
        let mut history = FrameHistory::default();
        let start = Instant::now();
        let mut end = 0.0;
        for n in 0..200 {
            let ms = if n < 2 { 40.0 } else { 10.0 };
            end += ms / 1000.0;
            history.push(1, Interval { end, ms }, start);
            if n % 5 == 0 {
                history.push(2, Interval { end, ms: 50.0 }, start);
            }
            if n == 98 {
                assert_eq!(history.sample(start).low_fps, None);
            }
        }
        let sample = history.sample(start);
        assert_eq!(sample.status, "live");
        assert_eq!(sample.fps, Some(100.0));
        assert_eq!(sample.low_fps, Some(25.0));
        let later = start + FRESH;
        assert_eq!(history.sample(later).fps, None);
        history.push(
            2,
            Interval {
                end: 15.0,
                ms: 50.0,
            },
            later,
        );
        assert_eq!(history.sample(later).fps, Some(20.0));
        assert_eq!(history.sample(later).low_fps, None);
    }

    #[test]
    fn rolling_low_forgets_old_stalls_and_rejects_duplicate_timestamps() {
        let mut history = FrameHistory::default();
        let now = Instant::now();
        for i in 0..1200 {
            let end = i as f64 / 100.0;
            history.push(
                1,
                Interval {
                    end,
                    ms: if i == 0 { 120.0 } else { 10.0 },
                },
                now,
            );
            history.push(1, Interval { end, ms: 100.0 }, now);
        }
        let sample = history.sample(now);
        assert_eq!(sample.low_fps, Some(100.0));
        assert!(history.chains[&1].frames.len() < 1010);
    }
}
