//! Linux 沙箱后端 — Bubblewrap + Seccomp BPF 过滤
//!
//! 【领域含义】基于 bwrap 的 Linux 原生沙箱方案，提供最强的隔离级别。
//! 通过命名空间隔离和 seccomp BPF 系统调用白名单实现：
//! - PID 命名空间（--unshare-pid）
//! - 网络命名空间（--unshare-net，网络禁用时）
//! - IPC 命名空间（--unshare-ipc）
//! - UTS 命名空间（--unshare-uts）
//! - 挂载命名空间 + 只读绑定挂载
//! - Seccomp BPF 系统调用白名单过滤
//!
//! 【核心职责】生成 BPF 过滤器 → 构建 bwrap 参数 → 派生隔离进程 → 读取输出。

#![cfg(target_os = "linux")]

use crate::shell::config::SandboxConfig;
use crate::shell::{ExecResult, SandboxError};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::time::timeout;
use tracing::debug;

// ---------------------------------------------------------------------------
// Seccomp BPF generation
// ---------------------------------------------------------------------------

/// AUDIT_ARCH_X86_64 in little-endian u32
#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xC000003E;

/// AUDIT_ARCH_AARCH64 in little-endian u32
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xC00000B7;

/// SECCOMP_RET_KILL: kill the process immediately (no notification)
const SECCOMP_RET_KILL: u32 = 0x0000_0000;
/// SECCOMP_RET_ALLOW: allow the syscall
const SECCOMP_RET_ALLOW: u32 = 0x7FFF_0000;

// BPF instruction opcodes
const BPF_LD_ABS: u16 = 0x20;
const BPF_JMP_JEQ: u16 = 0x15;
const BPF_RET: u16 = 0x06;

/// BPF 指令（sock_filter 兼容，8 字节）
///
/// 【领域含义】一条 seccomp BPF 过滤指令，包含操作码、跳转偏移和常量参数。
/// 【核心职责】作为 BPF 程序的基本构建块，描述对系统调用的匹配和动作。
#[repr(C, packed)]
struct BpfInstruction {
    /// BPF 操作码
    code: u16,
    /// 条件为真时的跳转偏移
    jt: u8,
    /// 条件为假时的跳转偏移
    jf: u8,
    /// 通用常量/参数
    k: u32,
}

/// BPF 程序构建器
///
/// 【领域含义】用于构建 seccomp BPF 过滤器的迷你 DSL，提供链式 API 生成字节码。
/// 【核心职责】提供 `ld_arch`、`ld_syscall_nr`、`jeq`、`ret_allow`、`ret_kill` 等高层指令方法。
struct BpfBuilder {
    instructions: Vec<BpfInstruction>,
}

impl BpfBuilder {
    fn new() -> Self {
        Self {
            instructions: Vec::new(),
        }
    }

    fn len(&self) -> usize {
        self.instructions.len()
    }

    fn emit(&mut self, code: u16, jt: u8, jf: u8, k: u32) {
        self.instructions.push(BpfInstruction { code, jt, jf, k });
    }

    fn ld_arch(&mut self) {
        self.emit(BPF_LD_ABS, 0, 0, 4);
    }

    fn ld_syscall_nr(&mut self) {
        self.emit(BPF_LD_ABS, 0, 0, 0);
    }

    fn jeq(&mut self, value: u32, jt: u8, jf: u8) {
        self.emit(BPF_JMP_JEQ, jt, jf, value);
    }

    fn ret_allow(&mut self) {
        self.emit(BPF_RET, 0, 0, SECCOMP_RET_ALLOW);
    }

    fn ret_kill(&mut self) {
        self.emit(BPF_RET, 0, 0, SECCOMP_RET_KILL);
    }

    fn into_bytes(self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.instructions.len() * 8);
        for ins in &self.instructions {
            buf.extend_from_slice(&ins.code.to_ne_bytes());
            buf.push(ins.jt);
            buf.push(ins.jf);
            buf.extend_from_slice(&ins.k.to_ne_bytes());
        }
        buf
    }
}

/// 构建 seccomp BPF 过滤器
///
/// 【领域含义】根据允许的系统调用号列表，生成 BPF 字节码过滤器。
/// 【核心职责】架构检查 → 系统调用号白名单匹配 → 未匹配则 KILL。
fn build_seccomp_bpf(allowed_syscalls: &[u32]) -> Vec<u8> {
    let mut b = BpfBuilder::new();

    // Architecture guard
    b.ld_arch();
    b.jeq(AUDIT_ARCH, 1, 0);
    b.ret_kill();

    // Syscall allowlist
    b.ld_syscall_nr();

    let n = allowed_syscalls.len();
    for (i, &nr) in allowed_syscalls.iter().enumerate() {
        let remaining = n - 1 - i;
        let jt = (remaining + 1) as u8;
        b.jeq(nr, jt, 0);
    }

    b.ret_kill();
    b.ret_allow();

    b.into_bytes()
}

// ---------------------------------------------------------------------------
// Allowed syscall sets (x86_64)
// ---------------------------------------------------------------------------

/// x86_64 架构的系统调用号定义
///
/// 【领域含义】按功能分类的系统调用号常量集合，用于构建 seccomp BPF 白名单。
/// 【核心职责】提供基础、写入、网络三类系统调用号，供 `allowed_syscalls` 按配置组合。
#[cfg(target_arch = "x86_64")]
mod syscalls_arch {
    /// 基础系统调用 — 所有命令都需要的最小集合
    pub const BASE: &[u32] = &[
        0,   // read
        1,   // write
        2,   // open
        3,   // close
        4,   // stat
        5,   // fstat
        6,   // lstat
        7,   // poll
        8,   // lseek
        9,   // mmap
        10,  // mprotect
        11,  // munmap
        12,  // brk
        13,  // rt_sigaction
        14,  // rt_sigprocmask
        15,  // rt_sigreturn
        16,  // ioctl
        17,  // pread64
        18,  // pwrite64
        19,  // readv
        20,  // writev
        21,  // access
        22,  // pipe
        23,  // select
        24,  // sched_yield
        25,  // mremap
        28,  // madvise
        32,  // dup
        33,  // dup2
        35,  // nanosleep
        39,  // getpid
        56,  // clone
        57,  // fork
        58,  // vfork
        59,  // execve
        60,  // exit
        61,  // wait4
        62,  // kill
        63,  // uname
        72,  // fcntl
        73,  // flock
        74,  // fsync
        75,  // fdatasync
        77,  // ftruncate
        78,  // getdents
        79,  // getcwd
        80,  // chdir
        89,  // readlink
        95,  // umask
        96,  // gettimeofday
        97,  // getrlimit
        102, // getuid
        104, // getgid
        107, // geteuid
        108, // getegid
        109, // setpgid
        110, // getppid
        131, // sigaltstack
        157, // prctl
        158, // arch_prctl
        160, // setrlimit
        186, // gettid
        202, // futex
        204, // sched_getaffinity
        217, // getdents64
        218, // set_tid_address
        219, // restart_syscall
        221, // fadvise64
        228, // clock_gettime
        229, // clock_getres
        230, // clock_nanosleep
        231, // exit_group
        232, // epoll_wait
        233, // epoll_ctl
        234, // tgkill
        257, // openat
        262, // newfstatat (fstatat64)
        263, // unlinkat
        264, // renameat
        267, // readlinkat
        269, // faccessat
        273, // set_robust_list
        281, // epoll_pwait
        290, // eventfd2
        291, // epoll_create1
        292, // dup3
        293, // pipe2
        302, // prlimit64
        318, // getrandom
        334, // rseq
    ];

    /// 文件写入相关系统调用 — 仅在 `allow_write=true` 时启用
    pub const WRITE: &[u32] = &[
        76,  // truncate
        82,  // rename
        83,  // mkdir
        84,  // rmdir
        85,  // creat
        86,  // link
        87,  // unlink
        88,  // symlink
        90,  // chmod
        91,  // fchmod
        93,  // fchown
        94,  // lchown
        265, // linkat
        266, // symlinkat
        268, // fchmodat
        280, // utimensat
        285, // fallocate
        316, // renameat2
        319, // memfd_create
    ];

    /// 网络相关系统调用 — 仅在 `allow_network=true` 时启用
    pub const NETWORK: &[u32] = &[
        41,  // socket
        42,  // connect
        43,  // accept
        44,  // sendto
        45,  // recvfrom
        46,  // sendmsg
        47,  // recvmsg
        48,  // shutdown
        49,  // bind
        50,  // listen
        51,  // getsockname
        52,  // getpeername
        53,  // socketpair
        54,  // setsockopt
        55,  // getsockopt
        288, // accept4
        299, // recvmmsg
        307, // sendmmsg
    ];
}

/// 非 x86_64 架构的空系统调用集合（占位）
///
/// 【领域含义】当目标架构不是 x86_64 时，所有系统调用集合为空。
/// 【核心职责】确保编译通过，实际运行时需要适配对应架构的调用号。
#[cfg(not(target_arch = "x86_64"))]
mod syscalls_arch {
    pub const BASE: &[u32] = &[];
    pub const WRITE: &[u32] = &[];
    pub const NETWORK: &[u32] = &[];
}

use syscalls_arch as syscalls;

/// 根据配置计算允许的系统调用列表
///
/// 【领域含义】根据 `allow_write` 和 `allow_network` 配置，组合基础 + 写入 + 网络系统调用。
/// 【核心职责】合并去重后返回排序的系统调用号列表，供 BPF 过滤器生成使用。
fn allowed_syscalls(config: &SandboxConfig) -> Vec<u32> {
    let mut s: Vec<u32> = syscalls::BASE.to_vec();

    if config.allow_write {
        s.extend_from_slice(syscalls::WRITE);
    }
    if config.allow_network {
        s.extend_from_slice(syscalls::NETWORK);
    }

    s.sort_unstable();
    s.dedup();
    s
}

// ---------------------------------------------------------------------------
// Bubblewrap execution
// ---------------------------------------------------------------------------

/// 在 Bubblewrap 沙箱中执行命令
///
/// 【领域含义】通过 bwrap 创建命名空间隔离的进程，应用 seccomp BPF 系统调用过滤。
/// 【核心职责】生成 BPF 过滤器 → 构建 bwrap 参数（命名空间、挂载、seccomp）→ 派生进程 → 读取输出。
pub async fn execute_in_bwrap(
    config: &SandboxConfig,
    command: &str,
    workdir: &Path,
) -> Result<ExecResult, SandboxError> {
    let start = Instant::now();
    let workdir = canonicalize_or_identity(workdir);

    let syscalls = allowed_syscalls(config);
    let bpf_bytes = build_seccomp_bpf(&syscalls);
    let seccomp_fd = write_bpf_to_fd(&bpf_bytes)?;

    let bind_flag = if config.allow_write {
        "--bind"
    } else {
        "--ro-bind"
    };

    // Build std::process::Command for pre_exec support.
    let mut std_cmd = std::process::Command::new("bwrap");
    std_cmd.arg("--unshare-pid");
    std_cmd.arg("--unshare-ipc");
    std_cmd.arg("--unshare-uts");
    if !config.allow_network {
        std_cmd.arg("--unshare-net");
    }
    std_cmd.arg("--proc").arg("/proc");
    std_cmd.arg("--dev").arg("/dev");
    std_cmd.arg("--tmpfs").arg("/tmp");
    std_cmd.arg(bind_flag);
    std_cmd.arg(workdir.as_os_str());
    std_cmd.arg(workdir.as_os_str());
    for allowed_path in &config.allowed_paths {
        let ap = canonicalize_or_identity(allowed_path);
        std_cmd.arg(bind_flag);
        std_cmd.arg(ap.as_os_str());
        std_cmd.arg(ap.as_os_str());
    }
    std_cmd.arg("--seccomp");
    std_cmd.arg(seccomp_fd.to_string());
    std_cmd.arg("--");
    std_cmd.arg("/bin/sh");
    std_cmd.arg("-c");
    std_cmd.arg(command);
    std_cmd.current_dir(&workdir);
    std_cmd.stdin(std::process::Stdio::null());
    std_cmd.stdout(std::process::Stdio::piped());
    std_cmd.stderr(std::process::Stdio::piped());

    debug!(
        target: "shell.linux",
        ?command,
        ?workdir,
        seccomp_fd,
        "spawning bwrap"
    );

    let std_child = std_cmd.spawn().map_err(|e| SandboxError::Spawn {
        backend: "bubblewrap",
        source: e,
    })?;

    // Close our copy of the seccomp FD.
    unsafe {
        libc::close(seccomp_fd);
    }

    let mut child = tokio::process::Child::from_std(std_child)
        .map_err(|e| SandboxError::Io { source: e })?;

    let timeout_dur = std::time::Duration::from_secs(config.timeout_secs);
    let wait_result = timeout(timeout_dur, child.wait()).await;

    let timed_out = wait_result.is_err();

    if timed_out {
        let _ = child.kill().await;
        let _ = child.wait().await;

        return Ok(ExecResult {
            stdout: String::new(),
            stderr: format!(
                "command timed out after {} seconds",
                config.timeout_secs
            ),
            exit_code: -1,
            duration_ms: start.elapsed().as_millis() as u64,
            timed_out: true,
        });
    }

    let status = wait_result
        .expect("timeout already handled")
        .map_err(|e| SandboxError::Io { source: e })?;

    let stdout = read_pipe_output(&mut child, StdioPipe::Stdout, config.max_output_bytes).await?;
    let stderr = read_pipe_output(&mut child, StdioPipe::Stderr, config.max_output_bytes).await?;

    let exit_code = status.code().unwrap_or(-1);
    let duration_ms = start.elapsed().as_millis() as u64;

    debug!(
        target: "shell.linux",
        exit_code,
        duration_ms,
        stdout_len = stdout.len(),
        stderr_len = stderr.len(),
        "bwrap completed"
    );

    Ok(ExecResult {
        stdout,
        stderr,
        exit_code,
        duration_ms,
        timed_out: false,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// 标准 I/O 管道标识
///
/// 【领域含义】区分标准输出和标准错误管道。
/// 【核心职责】在输出读取函数中指定要读取的目标管道。
#[derive(Copy, Clone)]
enum StdioPipe {
    /// 标准输出
    Stdout,
    /// 标准错误
    Stderr,
}

async fn read_pipe_output(
    child: &mut tokio::process::Child,
    which: StdioPipe,
    max_bytes: usize,
) -> Result<String, SandboxError> {
    let mut reader: Box<dyn AsyncRead + Unpin + Send> = match which {
        StdioPipe::Stdout => match child.stdout.take() {
            Some(p) => Box::new(p),
            None => return Ok(String::new()),
        },
        StdioPipe::Stderr => match child.stderr.take() {
            Some(p) => Box::new(p),
            None => return Ok(String::new()),
        },
    };

    let mut buf = Vec::with_capacity(4096);
    let mut total: usize = 0;

    loop {
        let remaining = max_bytes.saturating_sub(total);
        if remaining == 0 {
            let mut discard = [0u8; 4096];
            while reader
                .read(&mut discard)
                .await
                .map(|n| n > 0)
                .unwrap_or(false)
            {}
            break;
        }

        let chunk_size = remaining.min(4096);
        let mut chunk = vec![0u8; chunk_size];
        let n = reader
            .read(&mut chunk)
            .await
            .map_err(|e| SandboxError::Io { source: e })?;

        if n == 0 {
            break;
        }

        buf.extend_from_slice(&chunk[..n]);
        total += n;
    }

    let truncated = total > max_bytes;
    let mut s = String::from_utf8_lossy(&buf[..total.min(max_bytes)]).into_owned();

    if truncated {
        s.push_str("\n[... output truncated ...]");
    }

    Ok(s)
}

/// 将 BPF 字节码写入临时文件并返回文件描述符
///
/// 【领域含义】将 BPF 过滤器写入临时文件，以只读方式打开获取 fd，供 bwrap `--seccomp` 参数使用。
/// 【核心职责】写入临时文件 → 打开获取 fd → 删除临时文件 → 返回 fd。
fn write_bpf_to_fd(bpf: &[u8]) -> Result<std::os::unix::io::RawFd, SandboxError> {
    use std::io::Write;

    let tmp_dir = std::env::temp_dir();
    let mut path = tmp_dir.clone();
    path.push(format!("sandbox_seccomp_{}.bpf", std::process::id()));

    {
        let mut f = std::fs::File::create(&path).map_err(|e| SandboxError::Io { source: e })?;
        f.write_all(bpf).map_err(|e| SandboxError::Io { source: e })?;
        f.flush().map_err(|e| SandboxError::Io { source: e })?;
    }

    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| SandboxError::Config("invalid temp path".into()))?;

    let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY) };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        let _ = std::fs::remove_file(&path);
        return Err(SandboxError::Io { source: err });
    }

    let _ = std::fs::remove_file(&path);

    Ok(fd)
}

/// 路径规范化（失败时返回原路径）
///
/// 【领域含义】尝试将路径解析为绝对规范路径，失败时保持原样。
/// 【核心职责】确保挂载路径是绝对路径，避免 bwrap 挂载相对路径时的歧义。
fn canonicalize_or_identity(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// 检测 bwrap 是否可用
///
/// 【领域含义】检查系统中是否安装了 bubblewrap（bwrap）且可执行。
/// 【核心职责】通过 `which::which("bwrap")` 探测 bwrap 命令是否在 PATH 中。
pub fn is_bwrap_available() -> bool {
    which::which("bwrap").is_ok()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bpf_builder_empty() {
        let mut b = BpfBuilder::new();
        b.ret_allow();
        let bytes = b.into_bytes();
        assert_eq!(bytes.len(), 8);
        assert_eq!(&bytes[4..8], &[0x00, 0x00, 0xFF, 0x7F]);
    }

    #[test]
    fn test_bpf_builder_structure() {
        let mut b = BpfBuilder::new();
        b.ld_arch();
        b.jeq(0xC000003E, 1, 0);
        b.ret_kill();
        b.ld_syscall_nr();
        b.jeq(0, 1, 0);
        b.ret_kill();
        b.ret_allow();

        let bytes = b.into_bytes();
        assert_eq!(bytes.len(), 56);
        assert_eq!(b.len(), 7);
    }

    #[test]
    fn test_build_seccomp_bpf_non_empty() {
        let bpf = build_seccomp_bpf(&[0, 1, 3]);
        assert_eq!(bpf.len(), 9 * 8);
    }

    #[test]
    fn test_allowed_syscalls_base() {
        let config = SandboxConfig::default();
        let syscalls = allowed_syscalls(&config);
        assert!(!syscalls.is_empty());
        assert!(syscalls.contains(&0));
        assert!(syscalls.contains(&59));
        assert!(syscalls.contains(&60));
        assert!(syscalls.contains(&231));
    }

    #[test]
    fn test_allowed_syscalls_with_network() {
        let config = SandboxConfig {
            allow_network: true,
            ..Default::default()
        };
        let syscalls = allowed_syscalls(&config);
        assert!(syscalls.contains(&41));
        assert!(syscalls.contains(&42));
    }

    #[test]
    fn test_allowed_syscalls_with_write() {
        let config = SandboxConfig {
            allow_write: true,
            ..Default::default()
        };
        let syscalls = allowed_syscalls(&config);
        assert!(syscalls.contains(&83));
        assert!(syscalls.contains(&87));
    }

    #[test]
    fn test_allowed_syscalls_dedup() {
        let config = SandboxConfig {
            allow_write: true,
            ..Default::default()
        };
        let syscalls = allowed_syscalls(&config);
        let count = syscalls.iter().filter(|&&x| x == 77).count();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_bwrap_detection() {
        let _ = is_bwrap_available();
    }

    #[test]
    fn test_canonicalize_identity() {
        let p = Path::new("/nonexistent/path/12345");
        let result = canonicalize_or_identity(p);
        assert_eq!(result, p);
    }
}
