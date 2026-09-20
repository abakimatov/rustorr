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

### R3 — Engine spike и выбор движка

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

- Current stage: `R2` complete; next stage is `R3` engine spike and choice.
- Implementation status: Docker harness, deterministic fixture generation, known-torrent smoke and HTTP workload-matrix runner completed; one full matrix run is now recorded.
- Latest smoke evidence: `/tmp/rustorr-baseline/20260920T122608Z`; controlled netem applied and cleared successfully, with Range `206` in 579.7 ms; peer departure stopped the seeder successfully and the post-departure Range returned `206` in 3.9 ms. Reviewed aggregate: [`docs/benchmark-baseline.json`](benchmark-baseline.json).
- Next action: begin R3 engine spike using the R1 scenarios and the R2 contract constraints.

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
