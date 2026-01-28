//! Minimal vsock test - bypasses tokio_vsock to test raw vsock connectivity
//!
//! This uses raw libc calls to diagnose if the issue is with tokio_vsock or vsock itself.

use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;

fn main() {
    println!("[VSOCK_TEST] Starting minimal vsock test...");

    // AF_VSOCK = 40, SOCK_STREAM = 1
    const AF_VSOCK: libc::c_int = 40;
    const SOCK_STREAM: libc::c_int = 1;

    // Create vsock socket
    println!("[VSOCK_TEST] Creating vsock socket...");
    let fd = unsafe { libc::socket(AF_VSOCK, SOCK_STREAM, 0) };
    if fd < 0 {
        let err = std::io::Error::last_os_error();
        println!("[VSOCK_TEST] ERROR: Failed to create socket: {}", err);
        std::process::exit(1);
    }
    println!("[VSOCK_TEST] Socket created: fd={}", fd);

    // sockaddr_vm structure (from linux/vm_sockets.h)
    // struct sockaddr_vm {
    //     sa_family_t svm_family;     // AF_VSOCK (2 bytes)
    //     unsigned short svm_reserved1; // (2 bytes)
    //     unsigned int svm_port;      // (4 bytes)
    //     unsigned int svm_cid;       // (4 bytes)
    //     ...
    // }
    #[repr(C)]
    struct SockaddrVm {
        svm_family: u16,
        svm_reserved1: u16,
        svm_port: u32,
        svm_cid: u32,
        svm_flags: u8,
        svm_zero: [u8; 3],
    }

    let addr = SockaddrVm {
        svm_family: AF_VSOCK as u16,
        svm_reserved1: 0,
        svm_port: 6000,
        svm_cid: 2, // VMADDR_CID_HOST
        svm_flags: 0,
        svm_zero: [0; 3],
    };

    println!("[VSOCK_TEST] Connecting to CID 2, port 6000...");
    let ret = unsafe {
        libc::connect(
            fd,
            &addr as *const SockaddrVm as *const libc::sockaddr,
            std::mem::size_of::<SockaddrVm>() as libc::socklen_t,
        )
    };

    if ret < 0 {
        let err = std::io::Error::last_os_error();
        println!("[VSOCK_TEST] ERROR: Connect failed: {}", err);
        unsafe { libc::close(fd) };
        std::process::exit(1);
    }

    println!("[VSOCK_TEST] Connected successfully!");

    // Try to send a simple message
    let msg = b"Hello from vsock_test!";
    println!("[VSOCK_TEST] Sending test message...");
    let sent = unsafe {
        libc::send(fd, msg.as_ptr() as *const libc::c_void, msg.len(), 0)
    };

    if sent < 0 {
        let err = std::io::Error::last_os_error();
        println!("[VSOCK_TEST] ERROR: Send failed: {}", err);
    } else {
        println!("[VSOCK_TEST] Sent {} bytes", sent);
    }

    // Close socket
    unsafe { libc::close(fd) };
    println!("[VSOCK_TEST] Test complete!");
}
