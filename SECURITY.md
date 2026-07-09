# Security Policy

## Reporting a vulnerability

Please report suspected vulnerabilities privately via
[GitHub Security Advisories](https://github.com/Bramvzw/sooth/security/advisories/new)
for this repository, rather than opening a public issue. If that isn't
available, open a regular issue asking for a private channel and avoid
including exploit details until one is set up.

Include what you found, how to reproduce it, and the affected version. Expect
an initial response within a few days — this is a solo-maintained project.

## Supported versions

Only the latest released version is supported. There is no long-term support
branch at this stage.

## `sooth`'s own network and data promise

`sooth` makes zero network calls of its own: no telemetry, no update checks,
no crash reporting, no API keys, no accounts. It runs the test command you
give it, reads the JUnit XML that command produced on disk, and prints a
report — nothing leaves your machine.

(This is a statement about `sooth` itself. The network-egress *detection*
feature on the roadmap observes whether *your tests* make real network calls
— it does not change this promise about `sooth`.)
