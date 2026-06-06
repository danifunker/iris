#!/bin/bash
sudo sysctl kernel.perf_event_paranoid=-1
RUSTFLAGS="-C force-frame-pointers=yes" PERFFLAGS="-F 200 -g --call-graph dwarf" cargo flamegraph --profile profiling --features rex-jit,lightning --bin iris
