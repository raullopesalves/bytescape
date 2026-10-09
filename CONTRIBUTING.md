# Contributing to Bytescape

Thanks for wanting to help. Bug reports, ideas and pull requests are all
welcome.

## Reporting a problem

Open an [issue](https://github.com/raullopesalves/bytescape/issues/new/choose)
and pick "Something's wrong". The most useful things to include:

- Your Bytescape version (About) and Windows version.
- What you scanned (a drive, a folder, a network share) and whether
  Bytescape ran as administrator.
- What happened and what you expected.
- The end of the log file: About > Show the log file. It lists the folders
  you scanned and deleted, so look it over before you paste it.

## Building and testing

See "Building" in the [README](README.md). Before sending a pull request:

```
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
node crates/app/ui-test/run.mjs      # UI tests in headless Edge (Node 22+)
```

CI runs the same checks on Windows.

## How the code is laid out

`docs/HANDOFF.md` explains each part and the reasons behind it. In short:
the scanner and the categories are libraries with no UI code; the desktop
app keeps the scan in Rust and the front end (plain HTML, CSS and
JavaScript, no build step) asks for one folder at a time.

## Ground rules

- Never test deleting on real folders: tests use temporary folders.
- Don't copy code from WinDirStat (GPL) or WizTree (closed source).
- By contributing you agree that your work is licensed like the rest of
  the project: MIT OR Apache-2.0, at the user's option.
