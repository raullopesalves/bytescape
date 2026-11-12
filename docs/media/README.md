# Demo provenance

`bytescape-demo.webm` is a 51-second recording of `crates/app/ui-test/index.html`, the real frontend with its existing mock backend. It was captured in an isolated headless browser. Every drive statistic, file and path comes from `mock.js`; no real disk is scanned and no native delete command is available. Captions identify the recording as synthetic. The test harness version label is fixture data, not the current release version.

The screenshots in `../screenshots/` use the same isolated harness. They replace the earlier demo-drive captures so no named user profile or real mounted-drive list is shown. Mock timing and byte counts are illustrative and are not performance evidence.
