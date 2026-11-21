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

`sooth import --github` is the one command that reads anything remote, and it
does so by running the GitHub CLI (`gh`) you installed, with your own login:
`sooth` holds no token and opens no connection itself. It reads your
repository's Actions runs, jobs and job logs, and only when you run that
command.

(This is a statement about `sooth` itself. The network-egress *detection*
feature on the roadmap observes whether *your tests* make real network calls
— it does not change this promise about `sooth`.)

## Security model

`sooth` runs the test command you give it, in your environment, with your
privileges. It adds no privilege of its own and does not sandbox, escalate, or
alter the command. Trust a command passed to `sooth run` exactly as much as you
would trust running it directly.

The planned network-egress detection needs deeper access (traffic
interception) and will be strictly opt-in, Linux-first, and documented with its
own limitations and trust implications before it ships.

