# План реализации Rustorr

Статус: `planned`. Реализация не начата. Этот документ — рабочая точка продолжения между сессиями. Один этап получает статус `in progress` только после явного запуска работ; переход в `done` требует выполнения его критериев выхода и записи доказательств.

## Зафиксированный контекст

- Эталон совместимости: TorrServer MatriX.145, commit `2c7fa43b9ac64a9eda27314c0b6791518497f188`.
- Цель первой версии: существующие клиенты работают без изменений и доступен полный функциональный паритет эталона.
- Первая версия: Linux `x86_64` и `aarch64`, самостоятельный бинарник и Docker-образ.
- UI допускает новый TypeScript-интерфейс; предпочтение — React и Tailwind CSS.
- Начальная эксплуатация: удалённый сервер, 1–3 устройства, бюджет $10–14/месяц, до трёх прямых просмотров по четыре часа в день.
- Основной профиль проверки: 25 Мбит/с на прямой просмотр; RAM+SSD, RAM-only сохраняется для паритета.
- Приоритеты: время старта, перемотка, устойчивость; рост ресурсов допустим после пользовательских метрик.
- 100 одновременных просмотров — следующий этап; профиль — 100 прямых потоков по 25 Мбит/с, общий и 100 разных торрентов.
- Авторизация должна сохранять поведение TorrServer; поддерживаются встроенный TLS и работа за reverse proxy/VPN.
- Формат состояния Rustorr новый; импорт базы TorrServer не входит в первую версию.
- Выбор BitTorrent-движка не сделан. `librqbit` — первый кандидат для spike; сопровождаемый форк допустим при доказанном ограничении.

## Правила продолжения

1. Перед началом сессии прочитать этот файл, `docs/design-interview.md`, `docs/benchmark-baseline.md` и последний handoff этапа.
2. Не пересматривать принятые решения без нового измерения или явного изменения требований.
3. Не объявлять эффективность улучшенной без сравнения с тем же сценарием TorrServer.
4. Не считать API-совместимость доказательством совместимости плееров: проверять GET/HEAD/Range, плейлисты, заголовки и побочные эффекты.
5. После работы обновить статус этапа, артефакты, команды проверки, результаты и следующий handoff.
6. Если этап блокирован внешним выбором пользователя, зафиксировать блокер и продолжить независимые read-only задачи.

## Этапы

### R0 — Контекст и инвентаризация (`done` после проверки документов)

Цель: закрепить эталон, требования и область функционального паритета.

Артефакты: `CONTEXT.md`, `docs/design-interview.md`, ADR 0001–0002, `docs/research/torrserver-compatibility.md`, `docs/research/rust-engines.md`.

Выход: ссылки и commit эталона проверены; список функций и внешних контрактов составлен; открытые решения явно перечислены.

### R1 — Локальный baseline TorrServer (`done`)

Цель: получить воспроизводимые исходные показатели и эталонное поведение.

Задачи: подготовить Linux-окружение; запускать MatriX.145; создать детерминированные single/multifile torrents и локального seeder/tracker; ограничивать сеть; снять cold/warm start, seek, continuous playback, stalls, RSS/CPU/disk/network для 1 и 3 просмотров.

Артефакты: скрипты запуска, тестовые torrent-данные без закрытого контента, сырые результаты, агрегированный baseline с p50/p95 и долей остановок.

Проверки: повторный прогон даёт согласованные интервалы; отдельные результаты cold metadata и known `.torrent`; сценарии загруженной, отсутствующей и вытесненной области.

Выход: baseline опубликован; численные цели Rustorr сформулированы; шумы и ограничения эксперимента записаны.

Handoff: `baseline commit`, окружение, команды запуска, расположение результатов, последний успешный сценарий, незакрытые измерения.

### R2 — Characterization внешних контрактов (`done`)

Цель: превратить поведение эталона в автоматические совместимые проверки.

Задачи: покрыть `/torrents`, `/settings`, `/cache`, `/viewed`, `/stream`, `/play`, playlists, search, storage/TMDB/GStreamer/ffprobe endpoints; проверить JSON-типы и ошибки, GET/HEAD, Range/ETag/conditional requests, M3U, auth, CORS, WAF, TLS/proxy URL generation.

Отдельно зафиксировать Kodi headers, ForkPlayer `&fn=file.m3u`, VLC external tracks, а также побочный эффект HEAD на Viewed, если он подтверждён runtime эталона.

Артефакты: golden request/response corpus, семантический diff-инструмент, compatibility matrix, классификация обязательных и capability-specific контрактов.

Выход: Rustorr может запускать contract suite против эталона; различия не скрываются нормализацией динамических значений.

### R3 — Engine spike и выбор движка (`in progress`)

Цель: проверить `librqbit` и при необходимости альтернативы на реальных сценариях R1.

Задачи: закрепить версию кандидата; подключить минимальный поток `torrent → positional read/seek`; проверить DHT/PEX/uTP/trackers, metadata, priorities, cancellation, повторную загрузку вытесненных pieces и custom storage; измерить 1/3 потоков.

Выход: решение `adopt`, `fork` или `reject` с доказательствами; описаны upstream patches и стоимость сопровождения; выбранная политика хранения pieces совместима с cache semantics TorrServer.

### R4 — Архитектурный skeleton Rustorr

Цель: создать каркас workspace без полного функционального наполнения.

Задачи: определить crates/модули для engine adapter, torrent catalog, storage/cache, HTTP/API, auth/WAF, integrations, observability и UI; зафиксировать ownership данных и error boundaries; выбрать async runtime, HTTP stack, serialization и persistence по результатам R1–R3.

Выход: workspace собирается на Linux `x86_64` и `aarch64`; smoke server запускается бинарником и Docker; архитектурные границы записаны в ADR при наличии необратимого trade-off.

### R5 — Core torrent lifecycle и cache

Цель: реализовать управляемый жизненный цикл торрента и RAM+SSD cache для приоритетных playback-сценариев.

Задачи: add/load/drop, metadata, file selection, piece availability, reader ranges, preload/readahead, eviction, cancellation, restart persistence, global process budget и observability.

Выход: controlled torrent streams проходят seek/restart/eviction tests; кэш не превышает согласованные лимиты; результаты сравнимы с R1.

### R6 — HTTP streaming и совместимый API

Цель: сделать существующие клиенты работоспособными без изменений.

Задачи: воспроизвести routes, query flags, status schema, errors, GET/HEAD, ranges, ETag, MIME, playlists, absolute URLs, auth model, CORS/WAF, TLS and reverse proxy semantics.

Выход: contract suite R2 зелёная или каждое отличие имеет отдельное принятое решение; raw и GStreamer range behavior не смешаны без доказательства эквивалентности.

### R7 — Functional parity modules

Цель: перенести все возможности MatriX.145, сохраняя capability matrix платформ.

Порядок: web UI API integration; settings/viewed/storage; Torznab/search/TMDB; DLNA/Bonjour/MSX; WebDAV/FUSE; MCP; ffprobe; GStreamer HLS/remux/transcoding; remaining service/install behaviors.

Выход: каждая функция имеет implementation note, contract or smoke test, Linux x86_64/aarch64 status, external dependency note и known limitations.

### R8 — React/Tailwind UI

Цель: предоставить новый интерфейс без изменения серверных клиентских контрактов.

Задачи: TypeScript API client, torrent management, playback/file selection, settings, cache/viewed, search and capability-aware controls; responsive layout; build assets for binary and Docker packaging.

Выход: UI управляет Rustorr через R6 API; основной playback flow проходит в браузере и через M3U/external player links.

### R9 — Packaging, security и operations

Цель: подготовить пригодную удалённую установку.

Задачи: reproducible Linux builds for `x86_64`/`aarch64`; Docker multi-arch; config/env/volumes; built-in TLS; reverse proxy headers; auth/WAF defaults; structured logs, metrics and tracing; graceful shutdown and backup/restore of Rustorr state.

Выход: clean install, upgrade, restart and rollback smoke tests; documented resource and traffic assumptions; no secrets in images or logs.

### R10 — Performance gates и release candidate

Цель: доказать пользовательское улучшение и функциональный паритет для 1–3 устройств.

Задачи: повторить R1 against Rustorr; compare p50/p95 start/seek, stalls and resources; run full R2/R7 suites; test binary and Docker on both architectures; validate 4-hour daily budget assumptions locally, then on user-selected VPS.

Выход первой версии: all required MatriX.145 capabilities available on Linux targets, existing clients work without changes, user metrics improve against baseline or deviations are explicitly accepted, no unexplained playback stalls in agreed scenarios, release artifacts reproducibly built.

### R11 — Scale stage to 100 views

Цель: подтвердить следующий ориентир после первой версии.

Задачи: 100 direct streams at 25 Mbps; common torrent and 100 distinct torrents; vary cache hit rate and seek; evaluate vertical versus horizontal scaling only from measurements; calculate bandwidth and storage economics.

Выход: measured capacity envelope, bottleneck map, and a separate scaling ADR if architecture changes are required.

## Handoff template

Append this block to the stage file or this document after each session:

```md
### Handoff YYYY-MM-DD — Rn
- Status: planned | in progress | blocked | done
- Objective completed:
- Files/artifacts changed:
- Commands/tests run:
- Evidence and results:
- Decisions made:
- Open risks/questions:
- Exact next action:
- Starting directory/commit/config:
```

## Current checkpoint

- Current stage: `R3` complete; R0–R2 are complete; `R4` is next.
- Implementation status: Docker harness, deterministic fixture generation, known-torrent smoke and HTTP workload-matrix runner completed; one full matrix run is now recorded.
- Latest smoke evidence: `/tmp/rustorr-baseline/20260920T122608Z`; controlled netem applied and cleared successfully, with Range `206` in 579.7 ms; peer departure stopped the seeder successfully and the post-departure Range returned `206` in 3.9 ms. Reviewed aggregate: [`docs/benchmark-baseline.json`](benchmark-baseline.json).
- Next action: start R4 from [`docs/r3-continuation.md`](r3-continuation.md): production workspace and engine adapter over the adopted `librqbit 9.0.1`, the Rustorr-owned cache from [ADR 0004](adr/0004-cache-eviction-seam.md), then the R1 matrix against the Rustorr HTTP surface.

### Handoff 2026-09-20 — R1
- Status: done
- Objective completed: Docker baseline stack, deterministic single/multi-file torrent smoke path, full HTTP workload-matrix runner, controlled netem/peer-departure orchestration and per-scenario aggregation.
- Files/artifacts changed: `docker-compose.baseline.yml`, `tools/baseline/`, `.gitignore`, this checkpoint.
- Commands/tests run: `tools/baseline/r1.sh run` with Docker Desktop; `sh -n tools/baseline/r1.sh`; Python compile check; Compose config validation; fixture reproducibility diff.
- Evidence and results: `/tmp/rustorr-baseline/20260920T122608Z` and [`docs/benchmark-baseline.json`](benchmark-baseline.json); TorrServer MatriX.145 built from pinned commit; cold/warm/magnet/seek/1-view/3-view rows executed; netem apply/clear both returned code 0; seeder stop returned code 0; controlled rows returned HTTP `206`. Latest run recorded 0 failed requests, p50 `7.5 ms`, p95 `10814.8 ms`, and 12 transport stall-threshold events. Two-run aggregation records cold known-torrent p95 `10911.5 ms`, warm p95 `4976.2 ms`, seek-loaded p95 `4512.8 ms`, netem `579.7 ms`, and peer-departure recovery `3.9 ms`.
- Decisions made: Go entrypoint is `./server/cmd`; Docker build uses Go 1.25; tracker uses generated whitelist; seeder uses Transmission with existing-file verification.
- Decisions made: approved baseline-derived parity floors in `docs/benchmark-baseline.json`; accepted that eviction is represented by an explicit precondition because TorrServer exposes no deterministic eviction API in this harness; accepted that peer departure did not stall because the requested range was already available; transport stall proxy is not decoded-player rebuffering evidence.
- Open risks/questions: cold Range latency is currently measured separately from control-plane p50/p95; player-level rebuffering and deterministic eviction remain follow-up characterization concerns.
- Exact next action: start R2 contract characterization against the pinned TorrServer reference.
- Starting directory/commit/config: `/Users/keito/Documents/pets/rustorr`, working tree after `0741c0d`, Docker context `desktop-linux`, Compose file `docker-compose.baseline.yml`.

### Handoff 2026-09-20 — R2 start
- Status: in progress
- Objective completed: contract manifest, raw corpus runner, semantic diff and compatibility matrix skeleton added; deterministic Unicode/nested-path/external-track fixture added without changing the existing single fixture hash.
- Files/artifacts changed: `tools/contract/`, `tools/r2.sh`, `tools/baseline/generate-fixtures.py`.
- Commands/tests run: static checks plus Docker Compose reference stack; two control-plane captures with `tools/r2.sh capture reference --timeout 3 --only ...`; semantic diff; direct TorrServer Range probe; Docker status/log and fixture hash checks.
- Evidence and results: `/tmp/rustorr-contract/20260920T144518Z/reference.json` contains 34 cases with 0 request errors: control endpoints (200/204/400/404), raw GET/HEAD, single/suffix/open/multipart Range (`206`), M3U, GStreamer, CORS and MCP Streamable HTTP. Proxy capture `/tmp/rustorr-contract/20260920T145209Z/candidate.json` confirms forwarded-host M3U URLs and VLC external `.ac3`/`.srt` tracks through nginx on `127.0.0.1:8091`; TLS capture `/tmp/rustorr-contract/20260920T145806Z/candidate.json` has 0 errors and media M3U/ForkPlayer outputs contain only `https://` URLs through self-signed HTTPS on `8443`. Raw corpus preserves response bytes, hashes and JSON; normalization explicitly covers Date/Last-Modified, timestamps, peer/runtime counters, derived JSON Content-Length and generated multipart boundaries. The seeder fix `--encryption-tolerated` was validated by R1-compatible `connected_seeders=1` and HTTP `206`; clean repeatability was previously proven with `/tmp/rustorr-contract/20260920T135227Z*` and normalized `equal=true`.
- Decisions made: R2 corpus stores raw body bytes as base64 and decoded JSON together; raw and GStreamer probes remain separate scenarios; capability-specific endpoints remain visible when the reference returns 404.
- Open risks/questions: optional media routes remain capability-dependent; direct TorrServer built-in TLS startup was not exercised, while reverse-proxy TLS and forwarded URL generation are covered; same-container repeat requires recreating the seeder because a full-file read does not reliably restore its peer lifecycle; branch/draft-PR creation is blocked by sandbox `.git` write restrictions and invalid GitHub auth.
- Exact next action: accept the direct built-in TLS probe as a deployment follow-up or add a TLS-enabled TorrServer startup profile, then close R2 and hand off the contract suite to R6.
- Starting directory/commit/config: `/Users/keito/Documents/pets/rustorr`, current working tree after R1.

### Handoff 2026-09-20 — R3 start
- Status: in progress
- Objective completed: isolated `librqbit` spike project and Docker overlay added; final engine decision remains pending runtime evidence.
- Files/artifacts changed: `tools/engine-spike/`, `docker-compose.r3-engine.yml`, `docs/engine-spike.md`, `docs/adr/0003-bittorrent-engine-selection.md`.
- Commands/tests run: source inspection against pinned upstream API; shell validation and Docker-backed Rust checks are pending environment access.
- Evidence and results: probe records metadata, file mapping, positional read/seek, cancellation, persistence configuration and recording storage callbacks; raw results target `/tmp/rustorr-engine-spike`.
- Decisions made: `librqbit` is pinned to `9.0.1` for the first spike; the adapter is isolated from production HTTP/cache APIs; ADR 0003 is `proposed` until matrix evidence exists.
- Open risks/questions: current environment has no host `cargo/rustc` and Docker daemon access is unavailable; eviction/re-fetch, DHT/PEX/uTP and two-repeat performance matrix remain unverified.
- Exact next action: enable Rust/Docker execution, generate and commit `tools/engine-spike/Cargo.lock`, run the R1 matrix twice, then complete the storage eviction/re-fetch probe and ADR decision.
- Starting directory/commit/config: `/Users/keito/Documents/pets/rustorr`, `b816f5a`, baseline Compose plus `docker-compose.r3-engine.yml`.

### Handoff 2026-09-20 — R2 complete
- Status: done
- Objective completed: reproducible TorrServer contract suite with 34 reference scenarios, raw corpus, semantic diff, compatibility matrix, deterministic media fixtures, seeder reset orchestration, HTTP reverse-proxy profile and direct TorrServer TLS profile.
- Files/artifacts changed: `tools/contract/`, `tools/r2.sh`, `docker-compose.r2-proxy.yml`, `tools/baseline/docker/start-seeder.sh`, `tools/baseline/generate-fixtures.py`, `docs/implementation-plan.md`.
- Commands/tests run: Python compile, shell syntax, JSON validation, Compose validation; reference capture `/tmp/rustorr-contract/20260920T144518Z` with 34 cases and 0 errors; proxy HTTP/TLS captures `/tmp/rustorr-contract/20260920T145209Z` and `/tmp/rustorr-contract/20260920T145806Z`; direct TorrServer TLS capture `/tmp/rustorr-contract/20260920T150748Z` with 0 errors; clean repeatability `/tmp/rustorr-contract/20260920T135227Z*` with normalized `equal=true`.
- Evidence and results: API/control statuses, GET/HEAD, single/suffix/open/multipart Range, ETag/MIME/body capture, M3U with Unicode external tracks, VLC directives, ForkPlayer suffix, CORS, MCP, optional capability routes, reverse-proxy forwarded URLs and direct HTTPS behavior are recorded. The seeder uses `--encryption-tolerated` to interoperate with TorrServer's obfuscated peer handshake.
- Decisions made: dynamic values are normalized only through manifest-declared policy; raw response bytes and hashes remain preserved; optional 404 capabilities remain visible rather than filtered; clean repeat runs recreate the one-shot seeder lifecycle.
- Open risks/questions: production certificate rotation and auth deployment belong to R9; branch/draft-PR creation remains blocked by sandbox `.git` write restrictions and invalid GitHub auth.
- Exact next action: start R3 engine spike with the pinned R1 scenarios, preserving R2 contract constraints.
- Starting directory/commit/config: `/Users/keito/Documents/pets/rustorr`, Docker Compose baseline plus `docker-compose.r2-proxy.yml`, pinned TorrServer MatriX.145.

### Handoff 2026-09-20 — R3 implementation checkpoint
- Status: in progress
- Objective completed: isolated `librqbit` spike harness, recording storage factory, Docker Compose overlay and pinned Cargo dependency graph are implemented; no runtime gate has passed yet.
- Files/artifacts changed: `tools/engine-spike/`, `docker-compose.r3-engine.yml`, `docs/engine-spike.md`, `docs/adr/0003-bittorrent-engine-selection.md`, this plan, and generated `tools/engine-spike/Cargo.lock`.
- Commands/tests run: Compose config validation; Docker release build of `rustorr-engine-spike` with `librqbit 9.0.1`; binary `--help`; `sh -n tools/engine-spike/run.sh`; `git diff --check`. Rustfmt check was attempted but the image did not yet install the `rustfmt` component; Dockerfile was updated to install `rustfmt` and `clippy` for the next build.
- Evidence and results: build passed inside Docker's internal filesystem after a host bind-mounted cargo check hit rustc SIGBUS. The interrupted probe produced no valid measurement: `/tmp/rustorr-engine-spike/20260920T153748Z/probe.json` is empty; only an 8 MiB state file was created.
- Decisions made: `librqbit 9.0.1` and Rust `1.90.0` remain the pinned first candidate; ADR 0003 stays `proposed`; the empty probe is explicitly discarded and must not be used as evidence.
- Open risks/questions: probe lifecycle/flush behavior is unresolved; cancellation, custom-storage runtime callbacks, persistence, eviction/re-fetch, DHT/PEX/uTP and two-repeat performance matrix remain unverified.
- Exact next action: rebuild the image with rustfmt/clippy installed, run a bounded `--disable-dht` known-torrent probe, inspect its exit/JSON output, then continue with cancellation and eviction/re-fetch tests.
- Starting directory/commit/config: `/Users/keito/Documents/pets/rustorr`, working tree after the R3 spike files, Docker Compose baseline plus `docker-compose.r3-engine.yml`, raw attempt `/tmp/rustorr-engine-spike/20260920T153748Z`.

### Handoff 2026-09-20 — R3 runtime evidence
- Status: in progress
- Objective completed: bounded Docker runtime probe, deterministic known-torrent reads, seek reads, cancellation, recording-storage lifecycle, three concurrent views and session delete/re-add/refetch.
- Files/artifacts changed: `tools/engine-spike/`, `docker-compose.baseline.yml`, `docker-compose.r3-engine.yml`, `docs/engine-spike.md`, `docs/adr/0003-bittorrent-engine-selection.md`, this plan, and `tools/engine-spike/Cargo.lock`.
- Commands/tests run: release Docker build; rustfmt `--check`; clippy `--locked --release -- -D warnings`; Compose config validation; `sh -n tools/engine-spike/run.sh`; `git diff --check`; bounded probes with DHT disabled and raw output under `/tmp/rustorr-engine-spike/`.
- Evidence and results: two fresh known-torrent repeats matched SHA-256 `31e67ed8a3c058d5d68dfac1cd83c24b6ade45ffea0f7833d8eaa3951eae643b`; fresh seek matched `210ba6b19ee6a72f875261cd3a41d030fad18470c0fc633ee61b1a7d84174795`; three concurrent views matched; magnet metadata resolved and read matched; same-output persistence restart restored `262144` bytes with a `3.7 ms` read; custom storage recorded `creates=1`, `inits=1`, `takes=1`, `writes=20`, `reads=6`, `completed_pieces=1`; delete/re-add/refetch matched with two storage creations and two completed pieces; cancellation exited cleanly. The earlier stale-volume batch is excluded.
- Decisions made: `librqbit 9.0.1` remains the first candidate; the engine adapter remains isolated; session delete/re-add is accepted as a lifecycle gate but not as proof of piece-level eviction; same-output persistence fast-resume is evidenced but not generalized; ADR 0003 remains `proposed`.
- Open risks/questions: DHT/PEX/uTP capability and resource checks, piece-level cache eviction policy, and repeat performance parity against R1 floors remain open. The Unicode fixture is excluded from this probe because Transmission reports it as `0% None`.
- Exact next action: add the remaining capability-specific probes, run the agreed repeat performance matrix, then make the adopt/fork/reject decision in ADR 0003.
- Starting directory/commit/config: `/Users/keito/Documents/pets/rustorr`, current working tree after the R3 runtime evidence, Docker Compose baseline plus `docker-compose.r3-engine.yml`, raw results `/tmp/rustorr-engine-spike/`.

### Handoff 2026-09-21 — R3 capability and repeat matrix
- Status: in progress
- Objective completed: added observable DHT/listener capability output and `--views N` concurrent reads to the isolated librqbit probe; ran capability, persistence-relocation, repeat matrix and resource-accounting checks.
- Files/artifacts changed: `tools/engine-spike/src/main.rs`, `docs/engine-spike.md`, `docs/adr/0003-bittorrent-engine-selection.md`, `docs/r3-continuation.md`, this plan.
- Commands/tests run: Docker release build; `rustfmt --check`; `cargo clippy --locked --release -- -D warnings`; DHT bootstrap/client, TCP+uTP listener and uTP-only probes; persisted-session output relocation; two cold reads, two seek reads and two three-view reads; Docker stats sample; `git diff --check`.
- Evidence and results: `/tmp/rustorr-engine-spike/20260920T170225Z/probe.json` shows DHT enabled, listener bound and correct digest; `/tmp/rustorr-engine-spike/20260920T171757Z/probe.json` shows `live_utp=1` and `live_tcp=0`; `/tmp/rustorr-engine-spike/dht-two-peer-20260921T172000Z/client.json` shows `routing_table_size=58` and the expected digest; `/tmp/rustorr-engine-spike/persistence-restore-20260921T170500Z/` shows moved output causes fresh peer fetch; `/tmp/rustorr-engine-spike/repeat-20260921T170700Z/` has all expected digests for the repeat matrix and resource samples of RSS `6.16–6.52 MiB` and CPU `0.22–1.78%`.
- Decisions made: engine timings are repeatability evidence only and are not substituted for R1 HTTP/player parity; same-output fast-resume is not generalized; ADR 0003 remains `proposed`; PEX is not marked exercised because no public runtime counter exists.
- Open risks/questions: tracker-hidden two-peer PEX exchange, product cache eviction seam and a defined mapping from engine timings to R1 metrics remain open; DHT bootstrap discovery and uTP-only transfer now pass.
- Exact next action: build a two-peer capability harness, define the cache eviction seam and performance comparison, then make the adopt/fork/reject decision in ADR 0003.
- Starting directory/commit/config: `/Users/keito/Documents/pets/rustorr`, working tree after the R3 capability/repeat changes, Docker Compose baseline plus `docker-compose.r3-engine.yml`, raw results under `/tmp/rustorr-engine-spike/`.

### Handoff 2026-09-21 — R3 DHT and uTP gates
- Status: in progress
- Objective completed: closed the isolated DHT-bootstrap discovery and actual uTP-transfer gates for the librqbit spike; the production engine, routes, cache, and player integration are still not implemented.
- Files/artifacts changed: `tools/engine-spike/src/main.rs`, `docs/engine-spike.md`, `docs/r3-continuation.md`, `docs/adr/0003-bittorrent-engine-selection.md`, this plan.
- Commands/tests run: Docker release build; uTP-only probe; DHT bootstrap/client probe; final `cargo fmt --check`, `cargo clippy --locked --release -- -D warnings`, Compose config, shell syntax, and `git diff --check` checks.
- Evidence and results: `/tmp/rustorr-engine-spike/20260920T171757Z/probe.json` shows `live_utp=1`, `live_tcp=0` and the expected digest; `/tmp/rustorr-engine-spike/dht-two-peer-20260921T172000Z/client.json` shows `routing_table_size=58`, `outstanding_requests=41` and the expected digest; `/tmp/rustorr-engine-spike/persistence-restore-20260921T170500Z/` shows that moving the output causes a fresh peer fetch; `/tmp/rustorr-engine-spike/repeat-20260921T170700Z/` contains correct digests for the repeat matrix and resource samples.
- Decisions: DHT bootstrap and uTP transfer are evidenced; PEX exchange is not evidenced; ADR 0003 remains `proposed`; persistence relocation is a negative result; engine timings are repeatability evidence only and are not R1 HTTP/player parity.
- Open risks/questions: tracker-hidden two-peer PEX exchange, product-level cache eviction semantics, and an explicit engine-to-R1 metric comparison remain open.
- Exact next action: build the tracker-hidden PEX probe; define the cache-eviction seam independently of librqbit's missing piece-eviction API; define the comparison to R1 metrics; then make the adopt/fork/reject decision in ADR 0003.
- Starting directory/commit/config: `/Users/keito/Documents/pets/rustorr`, current working tree after the R3 capability/repeat changes, raw artifacts under `/tmp/rustorr-engine-spike/`.

### Handoff 2026-09-21 — R3 complete
- Status: done
- Objective completed: closed the three remaining R3 gates — tracker-hidden peer exchange, the Rustorr cache-eviction seam, and the engine-to-R1 metric comparison — and recorded the engine decision. Production routes, cache and player integration remain unimplemented.
- Files/artifacts changed: `tools/engine-spike/src/main.rs`, `tools/engine-spike/run.sh`, `docker-compose.r3-engine.yml`, `docs/adr/0003-bittorrent-engine-selection.md`, new `docs/adr/0004-cache-eviction-seam.md`, new `docs/engine-r1-metric-mapping.md`, `docs/engine-spike.md`, `docs/r3-continuation.md`, this plan.
- Commands/tests run: Docker release build; `cargo fmt --check`; `cargo clippy --locked --release -- -D warnings`; Compose config validation; `sh -n tools/engine-spike/run.sh`; `git diff --check`; two `run.sh pex` runs plus a no-initial-peer control; two `--evict-after-read` probes.
- Evidence and results: `/tmp/rustorr-engine-spike/pex-20260920T175048Z/` and `pex-20260920T175326Z/` — a client with session-level trackers and DHT disabled, LSD disabled and one initial peer discovered 3 unseen addresses and fetched `8,126,464` bytes / 31 pieces from the PEX-discovered Transmission seeder versus `262,144` bytes from its initial peer, returning the expected digest. Control `pex-control-20260920T174652Z/` found no peers and failed on the initialization deadline. `/tmp/rustorr-engine-spike/20260920T175558Z/` and `20260920T175634Z/` zeroed a verified 262,144-byte range under a live torrent; both re-reads returned the zeroed digest `8a39d2ab…` in `1.2 ms` and `0.9 ms` with no error and no re-fetch.
- Decisions made: ADR 0003 is `accepted` — adopt `librqbit 9.0.1` behind the Rustorr adapter, with piece invalidation named as the single fork trigger. ADR 0004 is `accepted` — Rustorr owns the cache at the `StorageFactory`/`TorrentStorage` seam and evicts at torrent granularity, because piece-level removal under a live torrent silently corrupts reads. `docs/engine-r1-metric-mapping.md` fixes the rule that engine timings may rule a candidate out but never declare parity.
- Defect found: `librqbit 9.0.1` accepts `AddTorrentOptions::disable_trackers` but never reads it; only `SessionOptions::disable_trackers` clears the tracker list. This invalidated the first PEX run, which the negative control caught. Worth reporting upstream; the adapter must assert isolation at session level.
- Correction within the same session: the first three "slower than TorrServer" rows were comparisons between different quantities. Re-measured under R1's own protocol — seek after a warm-up read is `500.2 / 505.2 ms` against a `4512.8 ms` floor (`20260920T181105Z`, `20260920T181129Z`), magnet metadata resolution alone is `2855.9 ms` against `6058.5 ms` (`20260920T181310Z`). A defect in the spike was found while doing this: `initialized_ms` was computed at the end of the run and reported total run time; it now stops after `wait_until_initialized`, with a new `total_ms` and `warmup_read` beside it. Real initialization is `1.4 ms`.
- Open risks/questions: no HTTP Range or player measurement exists for the candidate; the first read after a magnet resolution (`20597.9 ms`) and torrent-scoped re-fetch (`~20.1 s`) are unexplained and probably share a cause in peer re-acquisition; piece-level eviction stays impossible without an upstream change.
- Exact next action: start R4 — production workspace, engine adapter, Rustorr-owned cache per ADR 0004, then run `tools/baseline/r1.sh` against the Rustorr HTTP surface.
- Starting directory/commit/config: `/Users/keito/Documents/pets/rustorr`, working tree after the R3 closure changes, Docker Compose baseline plus `docker-compose.r3-engine.yml`, raw artifacts under `/tmp/rustorr-engine-spike/`.
