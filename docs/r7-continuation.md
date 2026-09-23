# Продолжение R7

Статус: `in progress`; R7.0–R7.2 выполнены 2026-09-23.

План и решения пользователя — в [`r7-plan.md`](r7-plan.md).

## R7.0 — инфраструктура наблюдения

- `tools/baseline/docker/Dockerfile.torrserver-r7` собирает тот же MatriX.145
  с `ffmpeg` (ffprobe), GStreamer 1.x (base/good/bad/ugly/libav) и `fuse3`.
  Точка входа `tools/contract/r7/reference-entrypoint.sh` кладёт свежую
  фикстурную базу `rutor.ls` в `/config`: эталон скачивает базу, только если
  локальная копия старше ~3 часов, поэтому остаётся офлайн. Запуск идёт с
  `--webdav`.
- `docker-compose.r7-capability.yml` подключает этот образ и заглушку
  Torznab-индексатора `indexer` (`172.31.250.30:9117`) во внутренней сети.
- `tools/contract/r7/fixtures.py` генерирует `rutor.ls` (raw DEFLATE с одним
  JSON-массивом записей) и элементы индексатора;
  `tools/contract/r7/fake_indexer.py` отвечает на `t=caps`/`t=search` и
  записывает входящие запросы (`GET /_requests` отдаёт и очищает журнал),
  так что корпус сравнивает и исходящие запросы сервера к индексатору.
- Раннер получил абсолютный `url` в шаге (для журнала индексатора) и
  `sleep_ms` — только для работы эталона без наблюдаемого признака
  готовности (загрузка базы Rutor после смены настроек).
- Профиль `r7` в `tools/contract/scenarios.json`: 34 сценария по API
  веб-интерфейса, хранилищу/TMDB, Rutor/Torznab, MSX, WebDAV, MCP, ffprobe и
  GStreamer; `tools/r2.sh capture <target> --profile r7`.

Первый действительный корпус эталона —
`/tmp/rustorr-contract/r7-reference-1/reference.json` (манифест `f3ccba99…`,
34 случая, 0 сбоев). Кандидат с тем же манифестом действителен; diff даёт 30
core-различий — модули ещё не реализованы.

## Наблюдения эталона, важные для реализации

- MCP работает без сессий: нет заголовка `Mcp-Session-Id`, `tools/list`
  отвечает и без `initialize`; ответы — `text/event-stream`.
- `HEAD /download/1` → `404` (маршрут только GET); `GET` с `Range` → `206`,
  нулевые байты, `Content-Range: bytes 0-15/1048576`.
- `/storage/settings` возвращает `{"settings","viewed","viewedCount"}`;
  `POST` → `{"status":"ok"}`.
- Torznab: исходящий запрос `apikey=…&cat=5000%2C2000&q=…&t=search` для
  `CatType` по умолчанию; размер форматируется с ошибкой эталона
  (`"4.4 GCiB"`), `pubDate` приводится к UTC; `/torznab/test` с неверным
  ключом — `200 {"error":"api error: Invalid API Key","success":false}`.
- WebDAV: `OPTIONS /dav/` — `Allow`, `DAV: 1, 2`, `MS-Author-Via: DAV`;
  `PROPFIND` → `207`.
- `/msx/` → `500` с пустым телом; `/files` → `""`.
- `/ffp/status` → `{"available":true}`; `/ffp/:hash/:id` на синтетической
  фикстуре → `400`, потому что это не медиафайл.
- GStreamer в этом образе не активен: `/gst/settings` →
  `{"built_in":false}`, остальные `/gst/*` → `404`.

## Открытые пробелы R7.0

- Для ffprobe и GStreamer нужна настоящая крошечная медиафикстура в раздаче
  сидера; выясняется в R7.7–R7.8, как и включение GStreamer у эталона.
- DLNA и Bonjour не HTTP: нужны отдельные пробы SSDP/mDNS и запуск с host
  network (R7.4).
- `r7-root` (старый UI эталона) уйдёт в allowlist с владельцем R8 по решению
  пользователя; `/stat` — текстовый дамп состояния движка anacrolix,
  стратегия сравнения решается в R7.1.
- Отложенные пробы профиля `direct` с несуществующими путями убираются по мере
  реализации модулей.

## R7.1 — API веб-интерфейса

- `/magnets` — HTML-список сохранённых торрентов в формате anacrolix
  `Magnet.String()`: `xt` первым, затем `dn` и `tr` в порядке ключей, значения
  экранированы как Go `url.QueryEscape`; порядок — по убыванию времени
  добавления, при равенстве по хешу (у эталона нестабильная сортировка).
- Политика трекеров MatriX.145 (`rustorr_lifecycle::trackers`):
  `RetrackersMode` 0 — свои, 1 — свои плюс `DefaultTrackers`, 2 — без
  трекеров, 3 — только `DefaultTrackers`; в любом режиме добавляется ярус из
  `<data-dir>/trackers.txt` (`udp`/`http`). Пустой `DefaultTrackers` заменяется
  встроенным списком. Движок получает трекеры по умолчанию и из файла при
  каждом добавлении (для magnet — через `&tr=`, librqbit игнорирует опцию).
  Ограничения: librqbit не может убрать собственные трекеры торрента, поэтому
  режимы 2 и 3 меняют `/magnets`, но не список анонсов движка; эталон
  фиксирует трекеры при добавлении, Rustorr вычисляет их по текущим
  настройкам; загрузка списка из зеркал ngosang/trackerslist пока не
  реализована (в герметичном стенде эталон тоже использует локальный список).
- `/download/:size` — нулевые байты с семантикой `ServeContent`: полный ответ,
  диапазон, multipart (boundary из 60 символов), `416`; `HEAD` → `404`;
  нечисловой размер → пустой `200`.
- `/shutdown` и `/shutdown/*reason` — корректная остановка тем же путём, что
  SIGTERM (`HttpConfig::shutdown`); проверено процессным тестом. Режим `--rdb`
  (`403` без причины) — в R7.2.
- `/stat` — собственный текстовый статус Rustorr; дамп anacrolix эталона не
  воспроизводится (allowlist `r7-stat`).
- `/` — короткая заглушка до R8 (allowlist `r7-root`, решение пользователя).
- `deferred-download` удалён из allowlist: маршрут реализован и совпадает с
  эталоном в профиле `direct`.

Доказательства: `/tmp/rustorr-contract/r7-r71/` — оба снимка `r7`
действительны (37 случаев); все сценарии `/magnets` и `/download` совпадают,
`r7-root`/`r7-stat` — ожидаемые различия, 26 core-различий остаются за
модулями R7.2–R7.8. `tools/r4.sh check` — 200 тестов.

## R7.2 — настройки, хранилище и режимы процесса

- `GET/POST /storage/settings`: `{"settings","viewed","viewedCount"}`, где
  `viewedCount` — число торрентов с просмотренными файлами; запись принимает
  JSON и `application/x-www-form-urlencoded`, проверяет значения
  (`Invalid settings/viewed storage value`, `No preferences provided`) и
  меняет только флаги `StoreSettingsInJson`/`StoreViewedInJson` без выгрузки
  торрентов. Хранилищем остаётся SQLite (ADR 0006): выбор сохраняется и
  отдаётся, но не переключает движок хранения.
- `GET /tmdb/settings` — секция `TMDBSettings` документа настроек.
- HEAD отвечает только там, где эталон регистрирует его явно или через `Any`
  (`/stream`, `/play`, `/dav`, `/mcp`, `/msx/proxy`); на остальных маршрутах —
  `404`, как у gin. Правило — одно middleware вместо проверок в обработчиках.
- Новые флаги (имена Rustorr; алиасы эталона `--rdb`/`--maxsize`/
  `--torrentsdir`/`--searchwa` — в R9):
  - `--read-only` / `RUSTORR_READ_ONLY` — режим эталона `-r`: запись
    каталога, настроек, просмотренного и WAF молча пропускается, `rem` и
    `wipe` ничего не удаляют, `settings set/def` не меняют и не выгружают
    торренты; `POST /waf` и `POST /storage/settings` → `403 {"error":"Read-only
    mode"}`, `/shutdown` без причины → `403`, `GET /waf` → `read_only: true`.
    Новый каталог данных при первом старте всё же инициализируется.
  - `--max-stream-size` / `RUSTORR_MAX_STREAM_SIZE` — стрим файла больше
    лимита (`/stream`, `/play`, включая HEAD) → `403` в формате Go
    `http.Error`: `file size exceeded max allowed N bytes`.
  - `--torrents-dir` / `RUSTORR_TORRENTS_DIR` — появившийся `*.torrent`
    сохраняется в каталог, выгружается из памяти и удаляется. Вместо fsnotify
    — опрос раз в секунду: файлы, лежавшие при старте, пропускаются, пока не
    изменятся (эталон реагирует только на события), файл берётся, когда его
    размер и время изменения стабильны два опроса подряд.
  - `--search-without-auth` / `RUSTORR_SEARCH_WITHOUT_AUTH` — передаётся в
    HTTP-слой; маршруты поиска появятся в R7.3.
- `--read-only`, `--max-stream-size` и `--torrents-dir` проверены unit- и
  HTTP-тестами по исходникам эталона; в чёрный ящик эти режимы не входят,
  потому что требуют отдельного запуска эталона с флагами.
- Раннер повторяет упавшую *подготовку* сценария один раз (с очисткой между
  попытками, попытки записаны в `lifecycle`); наблюдаемый запрос не
  повторяется. Причина — редкое зависание пиров у эталона в `pending`.

Доказательства: `/tmp/rustorr-contract/r7-r72c/` — оба снимка `r7`
действительны (41 случай; у эталона одна повторная подготовка
`r7-ffp-media`); все 8 сценариев хранилища и TMDB совпадают, остаются 22
core-различия модулей R7.3–R7.8. Регрессия `direct`
(`/tmp/rustorr-contract/r7-r72-direct/`) — 0 core-различий. `tools/r4.sh
check` — 206 тестов.

## Команды

```sh
docker compose -f docker-compose.baseline.yml -f docker-compose.r2-hermetic.yml \
  -f docker-compose.r7-capability.yml build torrserver
tools/r2.sh up
tools/r2.sh candidate-up
RUSTORR_CONTRACT_RUN_ID=r7-run tools/r2.sh capture reference --profile r7
RUSTORR_CONTRACT_RUN_ID=r7-run tools/r2.sh capture candidate --profile r7
tools/r2.sh diff /tmp/rustorr-contract/r7-run/reference.json \
  /tmp/rustorr-contract/r7-run/candidate.json /tmp/rustorr-contract/r7-run/diff.json
```
