# Bytescape: finding disk usage without hiding the cleanup decision

## Problem and scope

A large drive contains different kinds of space: personal files, installed software, caches, game assets and development dependencies. A useful disk tool must explain where the space goes and make risky removal a deliberate decision.

Bytescape is a Windows desktop app and CLI. This repository demonstrates product and implementation work across the scanner, categorisation rules, results interface, cleanup workflow, tests and packaging. It is an early release, not a claim of proven adoption or independent certification.

## Design decisions

| Decision | Reason and trade-off |
| --- | --- |
| Rust scan engine shared by desktop and CLI | Reuses filesystem and categorisation behaviour across two interfaces; Windows-specific capabilities still require platform code. |
| Parallel folder walk plus optional NTFS MFT scan | Normal users can scan without elevation; direct MFT access is an opt-in acceleration for an elevated process. |
| Flat node tree and SQLite scan history | Keeps large result sets manageable and supports comparisons; very large drives still consume memory and time. |
| Configurable category rules | Makes totals actionable while keeping classification inspectable; categories are guidance rather than proof that a file is disposable. |
| Review basket and checks at deletion time | Separates investigation from removal and handles paths that changed after the scan; users must still understand the selected files. |

## Walkthrough

[See the animated UI demo](../README.md#see-the-workflow): scan progress → results → folder navigation → search → cleanup basket. The frontend is real, but its backend is the existing test double. All paths and files are fictional, and the recording never scans or removes a real file. It demonstrates interaction, not native-system correctness or scan performance.

## Verification boundary

CI runs Rust formatting, Clippy and tests, followed by frontend interaction checks in headless Edge. The native Recycle Bin test is ignored by default. Signed updater metadata and published checksums are separate from Windows code signing: the installer remains unsigned.

Performance figures in the README are development observations on specific hardware. Do not generalise them to every drive, filesystem or machine. No usage numbers, testimonials or independent benchmark results are claimed.

## Try the product

[Windows downloads](https://github.com/raullopesalves/bytescape/releases/latest) · [README and usage](../README.md) · [Automated checks](https://github.com/raullopesalves/bytescape/actions)
