# R2 compatibility matrix

Reference: TorrServer MatriX.145 at `2c7fa43b9ac64a9eda27314c0b6791518497f188`.

| Area | Classification | Contract | Probe status |
| --- | --- | --- | --- |
| JSON API | required | actions, fields, types, empty arrays, nulls, errors and HTTP status | manifest |
| Streaming | required | GET/HEAD, Range, ETag, conditional requests, MIME and body bytes | manifest |
| Playlists | required | M3U URLs, ordering, filenames and external-player directives | manifest |
| Access | required | BasicAuth, CORS, WAF and forwarded host/proto behavior | manifest |
| Cache/viewed | required | state changes and HEAD side effects | manifest |
| Search/media | capability-specific | search, storage, TMDB, GStreamer and ffprobe | manifest |
| MCP | capability-specific | initialize response and protocol errors | manifest |
| DLNA/discovery | capability-specific | DLNA HTTP probe and Bonjour service | manifest; Bonjour remains external probe |
| WebDAV/FUSE | capability-specific | WebDAV OPTIONS and FUSE route availability | manifest |

`manifest` means the request is represented in `scenarios.json`; runtime status
is recorded only by a corpus capture. A `404` from a capability-specific
endpoint remains evidence and is not silently filtered from the corpus.
