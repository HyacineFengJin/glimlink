#!/bin/sh
# Run inside scripts/isolate.sh. Copy only source into a writable build directory.
set -eu
umask 077
mkdir -p /cache/workspace /reports
exec 9>/cache/workspace-build.lock
flock 9
rm -rf /cache/workspace/crates /cache/workspace/src /cache/workspace/tests /cache/workspace/examples
cp -p /workspace/Cargo.toml /cache/workspace/Cargo.toml
cp -p /workspace/build.rs /cache/workspace/build.rs
if [ -f /workspace/Cargo.lock ]; then cp -p /workspace/Cargo.lock /cache/workspace/Cargo.lock; fi
cp -pR /workspace/crates /cache/workspace/
cp -pR /workspace/src /cache/workspace/
if [ -d /workspace/tests ]; then cp -pR /workspace/tests /cache/workspace/; fi
if [ -d /workspace/examples ]; then cp -pR /workspace/examples /cache/workspace/; fi
cd /cache/workspace
# Export only an actually changed lock. A queued --locked failure must not
# overwrite the dependency resolver's newer report with its copied old lock.
runtime_lock_before=""
if [ -f Cargo.lock ]; then runtime_lock_before=$(sha256sum Cargo.lock); fi
trap 'if [ -f Cargo.lock ] && [ "$runtime_lock_before" != "$(sha256sum Cargo.lock)" ]; then cp Cargo.lock /reports/Cargo.lock; fi' EXIT
cargo "$@"
