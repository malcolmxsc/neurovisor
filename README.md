# NeuroVisor: Hybrid AI Agent Sandbox

**NeuroVisor** is a specialized Virtual Machine Manager (VMM) written in Rust, designed to securely orchestrate ephemeral AI agents.

Unlike general-purpose virtualization tools, NeuroVisor implements a **Hybrid Compute Architecture**: it runs lightweight "Agent Logic" (Python/LangChain) inside secure, stripped-down Firecracker microVMs, while offloading heavy inference tasks to the Host GPU via a high-performance **Virtio-Vsock** bridge.

This architecture mimics the production infrastructure of modern AI labs, prioritizing security, isolation, and inference latency.

---

## 🏗 Architecture

NeuroVisor operates as an asynchronous control plane interacting directly with the KVM hypervisor.

* **Host Logic (The Brain):** A Rust-based controller that manages VM lifecycles, creates TAP network interfaces, and acts as the **Inference Bridge** (forwarding prompts to local GPUs).
* **Guest Logic (The Agent):** A minimal Alpine Linux kernel (<300ms boot) with a pre-warmed Python runtime.
* **IPC Layer:** Zero-copy communication between Host and Guest using `virtio-vsock` (bypassing the TCP/IP stack overhead).

## 🚀 Key Features

### ⚡ Core Virtualization
* **Instant Boot:** Custom kernel configuration (`reboot=k`, stripped drivers) achieves sub-second boot times.
* **Software Defined Networking:** Manual implementation of TAP interfaces and raw IP routing logic in Rust.
* **Golden Image:** Immutable root filesystems pre-provisioned with AI runtimes (Python/NumPy) to eliminate installation latency.

### 🌐 Distributed Orchestration (Planned)
* **Custom Control Plane:** A centralized "Scheduler" service (Rust/Axum) that manages VM placement across multiple worker nodes.
* **gRPC Communication:** High-performance, strongly-typed internal communication using `tonic` (Protobuf).
* **Smart Scheduling:** Load-balancing algorithms (e.g., Least-Connections) to optimize resource usage across the cluster.

### 📊 Deep Observability & Tracing (Planned)
* **The "PLGT" Stack:** Full integration with **P**rometheus (Metrics), **L**oki (Logs), **G**rafana (Dashboards), and **T**empo (Traces).
* **Distributed Tracing:** Implementation of `TraceID` propagation (OpenTelemetry) to correlate requests from the User -> Controller -> Worker Node -> Kernel -> Python Agent.
* **eBPF Telemetry:** XDP probes to measure packet latency and OOM events at the kernel level.

### 🛡 Security & Isolation (Planned)
* **Seccomp Hardening:** Custom BPF profiles to restrict the Firecracker process syscalls (e.g., blocking `fork` or `exec`).
* **Network Isolation:** Strict "Metadata Blocking" rules to prevent SSRF attacks against cloud infrastructure.
* **Resource Throttling:** Token-bucket rate limiting on the Vsock channel.
---

## 🛠 Systems Engineering Stack

This project is built to demonstrate expertise in **Low-Level Linux Systems Engineering**:

* **Languages:** Rust (Control Plane), Python (Agent), C (eBPF).
* **Virtualization:** KVM, Firecracker, Virtio.
* **Kernel:** Boot Args, Tap/Tun Networking, Seccomp, cgroups.
* **Async Runtime:** Tokio, Hyper 1.0, Tonic (gRPC).

---

## 📦 Getting Started

### Prerequisites
* Linux Host (or WSL2 with KVM enabled)
* Rust Toolchain (Cargo)
* KVM Access (`/dev/kvm`)

### Running the VMM (Interactive Mode)
```bash
# 1. Setup Network (Host Side)
sudo ip tuntap add dev tap0 mode tap
sudo ip addr add 172.16.0.1/24 dev tap0
sudo ip link set tap0 up
sudo sh -c "echo 1 > /proc/sys/net/ipv4/ip_forward"

# 2. Launch NeuroVisor
cargo run --release
