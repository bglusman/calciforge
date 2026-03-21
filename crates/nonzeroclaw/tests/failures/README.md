# Proptest Counterexamples

No counterexamples were produced during the proptest run of 2026-03-21.

All 21 property-based tests passed across 5 channels (WhatsApp, Discord,
iMessage, Telegram, Signal) with the default 256 test cases per property.

The current allowlist implementations use exact string equality (`==`),
which correctly prevents substring/prefix matching bugs. No normalization
mismatches were found.

If future proptest runs produce counterexamples, they should be placed here
as `<channel>-<property>-counterexample.txt` with a 1-line summary of
the invariant violated.
