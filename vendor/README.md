# Pinned Darwin network declarations

`netdev` 0.46.3 and `netwatch` 0.19.3 are exact copies of their published
crates, including their licenses. Only their libc dependency entries change:
they resolve `rocky-apple-libc` instead of `libc`. Both workspace manifests
patch those exact crates; other netdev/netwatch versions remain upstream.

`apple-libc` reexports pinned upstream libc 0.2.190. On iOS it additionally
exports the Darwin routing constants and IPv6 ioctl types that libc declares
only for macOS. Its declarations are copied from libc 0.2.190's Darwin source,
under the included MIT/Apache licenses. The Darwin XNU network ABI is shared
by these targets: `in6_ifreq` has size 288, alignment 8 and its union at offset
16. Compile-time assertions check that layout on each iOS target.

No network implementation, route watcher, interface enumeration, endpoint
admission, feature or UniFFI version changes. The overlay has no authored
unsafe code. Vendored upstream dependency implementations retain their
original unsafe code and licenses, just as registry dependencies do.

This narrow arrangement resolves the missing `in6_ifreq` used by netdev and
the route constants used by netwatch, while preserving the locked iroh 1.3.0
path. See [qualification](../docs/runtime/kotlin-native-binding.md) for the
device/simulator build matrix and live simulator endpoint evidence.
