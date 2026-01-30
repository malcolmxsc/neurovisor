#!/bin/sh
# Snapshot init script for Neurovisor
#
# This script:
# 1. Sets up the system (mounts, /dev/vsock)
# 2. Sleeps to allow snapshot capture
# 3. After resume, runs guest_client
# 4. Powers off

mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev 2>/dev/null || true

# Create vsock device
rm -f /dev/vsock
mknod /dev/vsock c 10 122
chmod 666 /dev/vsock

echo "========================================"
echo "  Neurovisor Guest VM - SNAPSHOT INIT"
echo "========================================"
echo "[SNAPSHOT_INIT] System setup complete"
echo "[SNAPSHOT_INIT] Sleeping 5s (snapshot taken during this)..."

# Sleep for 5s - builder pauses at 3s, so snapshot is taken mid-sleep
# On restore, VM continues from where it was (still sleeping)
# The remaining ~2s gives the host gRPC server time to be ready
sleep 5

echo "[SNAPSHOT_INIT] Continuing (fresh boot or snapshot restore)..."

echo "[SNAPSHOT_INIT] Running guest_client..."
/usr/local/bin/guest_client "Hello from snapshot restore!" 2>&1
echo "[SNAPSHOT_INIT] guest_client exited with code: $?"

echo "[SNAPSHOT_INIT] Shutting down..."
sync
poweroff -f
