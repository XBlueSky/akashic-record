# Security Policy

## Reporting a vulnerability

**Please do not report security vulnerabilities through public GitHub issues,
discussions, or pull requests.**

Instead, use GitHub's **private vulnerability reporting**: on the repository's
**Security** tab, choose **Report a vulnerability**. This opens a private
advisory visible only to you and the maintainers.

Please include:

- a description of the vulnerability and its impact;
- the affected component (backend crate, frontend, or infrastructure) and
  version / commit;
- steps to reproduce, and a proof of concept if you have one;
- any suggested remediation.

## What to expect

- We aim to acknowledge a report within a few days.
- We'll work with you to understand and validate the issue, keep you updated on
  progress, and coordinate disclosure timing.
- We'll credit reporters who wish to be credited once a fix is available.

## Supported versions

Akashic Record is self-hosted and pre-1.0; security fixes target the latest
`master`. If you run a pinned build, note the commit in your report so we can
assess whether it is affected.

## Scope notes

Akashic Record is designed to be self-hosted. Its MCP read tier is
anonymous-callable by design (see the README's "MCP write tool authentication"
section) — reachability of the MCP SSE port is a deployment/network concern for
the operator. Reports about the documented anonymous read surface should focus
on cases where it exposes data or capability beyond that documented contract.
