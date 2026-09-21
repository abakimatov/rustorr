# R4 continuation state

Updated: `2026-09-21`

Это точка входа для продолжения R4 в новой сессии. План этапа и журнал шагов
1–7 — в [`r4-plan.md`](r4-plan.md); здесь состояние, команды, ловушки и точный
следующий шаг.

## Current status

R4 `done`: **шаги 1–9 выполнены**. `tools/r4.sh check` зелёный (165 тестов),
`tools/r4.sh cross-build` подтвердил `x86_64`, а `tools/r4.sh smoke` собрал
обе архитектуры и проверил lifecycle контейнера. R2 corpus лежит в
`/tmp/rustorr-contract/r4-20260921/`: `/echo` совпадает с reference;
25 остальных расхождений — намеренно не реализованные маршруты R6.

Ветка `r3-engine-spike`, HEAD `761589a`. **Вся работа R4 не закоммичена**:
3 изменённых файла (`.gitignore`, `docs/implementation-plan.md`,
`docs/r3-continuation.md`) и 52 неотслеживаемых файла (`Cargo.toml`,
`Cargo.lock`, `rust-toolchain.toml`, `crates/`, `tools/r4.sh`, `tools/r4/`,
`docs/r4-plan.md`, этот файл). **2026-09-21 пользователь отказался
коммитить.** Не предлагать повторно: работа остаётся в рабочем дереве, пока
он сам не попросит; тогда же он выберет ветку (текущая называется
`r3-engine-spike`).

## Read first

1. [`r4-plan.md`](r4-plan.md) — цели, стек, границы crates, шаги и журнал
   выполнения с доказательствами и найденными ошибками.
2. [`adr/0003-bittorrent-engine-selection.md`](adr/0003-bittorrent-engine-selection.md)
   и [`adr/0004-cache-eviction-seam.md`](adr/0004-cache-eviction-seam.md) —
   принятые решения, на которых стоит код.
3. [`implementation-plan.md`](implementation-plan.md) — handoff
   `2026-09-21 — R4 steps 1–7`.

## How to work

На хосте нет `cargo`. Всё идёт через Docker; образ `rustorr-r4-dev`
(`tools/r4/Dockerfile.dev`), тома `rustorr-r4-target` и `rustorr-r4-cargo`.

```sh
tools/r4.sh doctor        # Docker доступен?
tools/r4.sh check         # границы crates + openssl-страж, fmt, clippy -D warnings, тесты
tools/r4.sh boundaries    # только границы и openssl
tools/r4.sh test          # только тесты
tools/r4.sh build         # release-сборка всего workspace
tools/r4.sh cargo <args>  # любая команда cargo в контейнере
tools/r4.sh clean         # удалить тома с target и кэшем cargo
```

Ожидаемый результат `check` на момент handoff: код 0, `boundaries ok: 6 crates`,
тесты: cache 52, domain 20, engine 25, http 12, state 36, server 11 модульных и
9 процессных.

## Repository map

| Путь | Что там |
| --- | --- |
| `crates/rustorr-domain` | `InfoHash`, `FileIndex` (ноль/единица только через `from_one_based`/`one_based`), `PieceIndex`, `ByteRange`, `Error` |
| `crates/rustorr-engine` | порт `Engine`, `EngineConfig`, `LibrqbitEngine`; мост `CacheStorageFactory`/`CacheStorage`; `session_tests.rs` — сквозные тесты на настоящем librqbit |
| `crates/rustorr-cache` | `PieceStore`, `MemoryStore`, `DiskStore`, `Cache`, `Pin`, `TorrentLayout`; двухфазное вытеснение |
| `crates/rustorr-state` | `State` (SQLite), `CatalogEntry`, `ViewedEntry`; схема v1, миграции |
| `crates/rustorr-http` | `router`, `serve`, `ServerInfo`, `ApiError`; порядок слоёв в `with_layers` |
| `crates/rustorr-server` | бинарник `rustorr`: `config.rs`, `logging.rs`, `run.rs`, `main.rs`, `tests/process.rs` |
| `tools/r4.sh`, `tools/r4/` | команды проверки, `Dockerfile.dev`, `check-boundaries.py` |
| `tools/engine-spike/` | инструмент доказательств R3; вне workspace (`exclude`) |

Запуск: `rustorr --help`. Значения по умолчанию: адрес `0.0.0.0:8090`, data dir
`data`, кэш на диске 4 GiB (заглушка), срок остановки 5 с. Раскладка data dir:
`rustorr.db`, `engine/` (`scratch/`, `dht.json`), `cache/<hash>/<file>`.
Переменные: `RUSTORR_LISTEN`, `RUSTORR_DATA_DIR`, `RUSTORR_CACHE`,
`RUSTORR_CACHE_SIZE`, `RUSTORR_PEER_PORT`, `RUSTORR_DISABLE_DHT`,
`RUSTORR_DISABLE_TRACKERS`, `RUSTORR_SHUTDOWN_GRACE`, `RUSTORR_LOG_FORMAT`.

## Pitfalls

Каждая из них стоила времени в шагах 1–7.

- **`--locked` падает после добавления зависимости.** Один раз запустить
  `tools/r4.sh cargo check --workspace --all-targets` без `--locked`, дальше
  использовать `check`. `Cargo.lock` засеян lock-файлом spike, чтобы
  транзитивные версии, прошедшие R3, не поехали.
- **Первый `cargo fetch` может зависнуть** на `Updating crates.io index`
  (был один раз, 5 минут). Убить контейнер и повторить: со второго раза 14 с.
- **Не монтировать `target/` с хоста**: `rustc` падает с SIGBUS (R3). Только
  именованные тома.
- **Мутационная проверка.** Результат мутации засчитывается, только если
  сборка прошла: код 101 при нуле упавших тестов — это ошибка компиляции.
  Один раз я записал бы это как «поймано».
- **Стабильность теста — отдельная проверка.** «Прошёл один раз» ничего не
  значит: тест логов в `rustorr-http` падал в 15% параллельных запусков.
  Гонять тестовый бинарник в цикле (см. `r4-plan.md`, шаг 7).
- **`env!("CARGO_BIN_EXE_rustorr")` зашит на этапе сборки** как
  `/cache/target/debug/rustorr`; запускать процессные тесты вне `r4.sh` нужно
  с томом, смонтированным в `/cache/target`.
- **Процессным тестам нужен `kill`.** В образе `rustorr-r4-dev` он есть; в
  `unsafe`-запрещённом коде (`unsafe_code = "forbid"`) сигналов иначе не
  послать.
- **zsh на хосте:** `echo ======` — ошибка (подстановка команды); `PIPESTATUS`
  нет, код возврата после конвейера — код последней команды; `cd` в команде
  меняет каталог следующих вызовов, использовать абсолютные пути.
- **Артефакты в `/tmp` не в репозитории** и пропадают после перезагрузки:
  эталонный корпус `/tmp/rustorr-contract/20260920T144518Z/reference.json`,
  результаты R1 и R3. Если нужны, воспроизводятся командами R1–R3.

## Historical step 8 plan (completed)

### Шаг 8 — Docker и smoke

1. **`Dockerfile`** (multi-stage) и `.dockerignore`. Сборка на
   `--platform=$BUILDPLATFORM` с кросс-компиляцией под `$TARGETPLATFORM`
   (`aarch64-unknown-linux-gnu` и `x86_64-unknown-linux-gnu`), не сборка под
   QEMU. Нужны `rustup target add`, кросс-компилятор и `CC_<triple>` /
   `CARGO_TARGET_<TRIPLE>_LINKER` для crate с нативным кодом: **`aws-lc-sys`**
   (rustls и SHA-1) и **bundled SQLite** (`libsqlite3-sys`). Это главный риск
   шага; проверить, нужен ли `cmake`. Запасной вариант — сборка `amd64` под
   эмуляцией (медленно). Рантайм: `debian:bookworm-slim`, **пользователь не
   root**, `/data` создан и принадлежит ему (иначе свежий именованный том
   окажется root-овым и запись упадёт), `EXPOSE 8090`, `ENV
   RUSTORR_DATA_DIR=/data RUSTORR_LISTEN=0.0.0.0:8090`. Проверить, нужен ли
   `ca-certificates` (rustls берёт системные корни для HTTPS-трекеров).
2. **`docker-compose.r4-smoke.yml`** и команда **`tools/r4.sh smoke`**: сборка
   `linux/arm64` и `linux/amd64`, запуск, `GET /echo`, `docker stop` → код
   выхода 0 (`docker inspect … State.ExitCode`) и строка `shutdown complete` в
   логе, повторный старт на том же томе (в логе `schema_version=1`).
   Порты хоста 8090, 8091, 8092, 8443, 8444 заняты стендом R1/R2 — для Rustorr
   взять, например, 8095.
3. **Сценарий `/echo`.** Добавить в `tools/contract/scenarios.json`
   (`area` — access или новая, `classification: required`), поднять эталон
   (`tools/r2.sh up`, образ `rustorr-r1-torrserver` есть локально),
   `RUSTORR_RESET_SEEDER=1 tools/r2.sh capture reference`. Проверить формат
   ответа и `Content-Type`; сейчас `rustorr` отдаёт `rustorr 0.1.0`
   (`text/plain; charset=utf-8`) по памяти, не по корпусу.
4. **R2 против skeleton.** Запустить Rustorr, затем
   `RUSTORR_CANDIDATE_BASE_URL=http://127.0.0.1:8095 tools/r2.sh capture candidate --readiness-timeout 1`.
   Без `--readiness-timeout 1` runner после сценариев `torrents-add-known` и
   `torrents-add-media` ждёт готовности торрента по 90 с (у кандидата нет
   `/torrents`). Затем `tools/r2.sh diff <reference> <candidate>`. Ожидаемо
   совпадут только сценарии с 404 (`missing-route`, `settings-get`,
   `viewed-get`, `cache-get`, …) и `echo`; остальное разойдётся, и это исходная
   точка R6, а не провал. `diff.py` показывает отсутствующий с одной стороны
   сценарий как `missing-case`. Сводку сохранить в `docs/`, сырьё оставить в
   `/tmp`.
5. Проверить `aws-lc-sys` и SQLite под `x86_64` **до** остального Docker-кода
   (быстрый `cargo build --target x86_64-unknown-linux-gnu` в контейнере с
   кросс-тулчейном), чтобы риск не всплыл в конце.

### Шаг 9 — документы (completed)

- **ADR 0005** — границы crates и изоляция движка: таблица допустимых рёбер и
  её автоматическая проверка, `PieceStore` как шов на стороне Rustorr,
  двухфазное вытеснение, мягкий лимит кэша как следствие ADR 0004.
- **ADR 0006** — SQLite как источник истины: `rusqlite` bundled,
  `user_version`/`application_id`, WAL, `synchronous` по умолчанию,
  persistence и fast-resume librqbit выключены, основа для backup в R9.
- Архитектурный документ: crates и владение данными, границы ошибок, порядок
  запуска и остановки, порядок HTTP-слоёв, раскладка data dir.
- Обновить `implementation-plan.md` (R4 → `done`, handoff),
  `r3-continuation.md` и этот файл; проверить критерии выхода в `r4-plan.md`.

## Carried forward

**R5 — жизненный цикл и кэш**
- Подключить `CacheStorageFactory` к сессии (`LibrqbitEngine::start`).
- Координатор вытеснения: pin на просмотр, `eviction_candidates`, сначала
  `Session::delete`, потом `Cache::remove`.
- Решить, что делать с мягким лимитом (торрент с живым просмотром не
  вытесняется); жёсткая граница — это piece-level invalidation, то есть fork
  trigger из ADR 0004.
- Сохранение `DiskStore` между рестартами; открытие файла на каждую операцию.
- Прогнать `tools/baseline/r1.sh` против Rustorr на продовой сборке: это же
  закроет то, что гейты R3 шли на SHA-1 через OpenSSL, а продовая сборка — на
  `aws-lc-rs`.
- Гипотеза: R3-шные ~20 с при повторной загрузке — это заново найденные
  пиры (там не передавались `initial_peers`), а не storage: на loopback с
  явным пиром перекачка заняла ~45 мс.
- Влияние `dht.json` на холодный старт, двухуровневый RAM+SSD store.

**R6 — совместимый API**
- Формат `/echo` и решение о том, как Rustorr представляется клиентам.
- Ошибки нижних crates → ответы, по маршрутам и из корпуса; тело `500` при
  панике (сейчас пустое, допущение); поведение HEAD.
- `index=0` в запросах эталона не характеризован; каскад viewed при
  удалении торрента; тип `timecode` (в схеме `REAL`); поле `data` каталога
  не подтверждено корпусом; записи каталога без метаданных
  (`metainfo NOT NULL`).
- CORS, WAF, auth встают между трассировкой и перехватом паник в
  `with_layers`.

**R9 — эксплуатация**
- `net.core.rmem_max` (librqbit просит ~160 МиБ, получает 8 МиБ).
- WAL на сетевой ФС и bind mount Docker; `synchronous`; online backup.
- Фильтрация логов librqbit; флаги совместимости с CLI TorrServer;
  соотношение `--shutdown-grace` и таймаута `docker stop`.

## Decisions taken by the user

Решено 2026-09-21, повторно не спрашивать:

- **Коммит не делать.** Работа R4 остаётся в рабочем дереве. Риск: всё
  лежит только на диске, поэтому перед рискованными операциями (кросс-сборка,
  правки Docker) полезно сделать копию каталога, но без коммита.
- **`/echo` остаётся допущением** до тех пор, пока сценарий не снят с
  эталона (шаг 8, пункт 3). Формат `rustorr 0.1.0` (`text/plain;
  charset=utf-8`) взят по памяти об API TorrServer, не из корпуса R2;
  пользователь это принял. Закрывается снятием сценария и сверкой, а не
  обсуждением.

Открытых вопросов к пользователю нет.

R4 закрыт. Точка входа продолжения — R5 из раздела **Carried forward**:
подключить `CacheStorageFactory` к сессии, добавить lifecycle coordinator и
измерить R1 через Rustorr HTTP. Перед началом перечитать
[`r4-architecture.md`](r4-architecture.md) и ADR 0004–0006.
