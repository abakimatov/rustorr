# Продолжение R7

Статус: `in progress`; R7.0–R7.8 выполнены 2026-09-24 (R7.6 MCP отложен).

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

## R7.3 — поиск

- Новый крейт `rustorr-search` (граница: без зависимостей от других крейтов
  workspace; HTTP и сервер зависят от него) с портом `Search`, который HTTP
  получает так же, как `ClientCore`.
- Rutor: `<data-dir>/rutor.ls` — raw DEFLATE с JSON-массивом записей (битые
  записи пропускаются). Анализатор эталона: токены по не-буквам/не-цифрам,
  нижний регистр, `ё`→`е`, стоп-слова; объявленный стеммер эталон не
  вызывает, мы тоже. Поиск — пересечение ID по всем токенам; сортировка — по
  расстоянию Левенштейна (по символам) между `ClearStr(query)` и
  `ClearStr(Name+Names)+Year`, при равенстве — новее раньше. Включение следует
  `EnableRutorSearch` при старте и после каждого `settings set/def`: если
  локальная копия старше 175 минут, база скачивается с
  `http://releases.yourok.ru/torr/rutor.ls`, затем обновляется раз в 3 часа;
  выключение выгружает базу.
- Torznab: запрос `apikey`, `cat`, `q`, `t` в порядке ключей Go
  (`cat=5000,2000` по умолчанию, `Categories` для `manual`, без фильтра для
  `all`); XML разбирается по локальным именам; размер с ошибкой единиц
  эталона (`KCiB`/`MCiB`/`GCiB`); дата `RFC1123`/`RFC1123Z` → RFC 3339 со
  смещением; `index` выбирает индексатор, иначе результаты всех по порядку.
  `/torznab/test`: `{"success":true}` или `{"error":…, "success":false}` с
  текстами эталона; неверное тело — пустой `400`. Таймаут запросов — 60 с
  (у Go-клиента эталона таймаута нет).
- `/search` и `/torznab/search` (с путём и без): `400 []` при выключенном
  поиске, запрос декодируется дважды, как `c.Query` + `url.QueryUnescape`;
  без авторизации при `--search-without-auth`.
- Все JSON-ответы теперь экранируют `<`, `>`, `&`, U+2028 и U+2029 как Go
  `encoding/json` (исправляет и латентное расхождение R6).
- Стенд: `docker-compose.r7-candidate.yml` одноразовым сервисом кладёт
  фикстурную `rutor.ls` в каталог данных кандидата.
- axum-шаблон `/search/{*query}` не принимает пустой хвост, поэтому
  `/search/` и `/torznab/search/` зарегистрированы отдельно (у gin
  catch-all покрывает все три формы).

Доказательства: `/tmp/rustorr-contract/r7-r73b/` — оба снимка `r7`
действительны (45 случаев); все 12 сценариев поиска совпадают, включая
исходящие запросы к индексатору; остаются 14 core-различий модулей
R7.4–R7.8. `tools/r4.sh check` — 219 тестов.

## R7.4 — MSX

- Модуль `rustorr_http::msx_api` с состоянием `Msx`; HTTP-слой получает
  интеграции одной структурой `Integrations { search, msx }` (следующие
  модули добавляются туда же). Все маршруты — за авторизацией управления.
- Исходящие запросы Rutor, Torznab и MSX идут через один `reqwest::Client`
  сервера: таймаут соединения 30 с и чтения 60 с без общего лимита, чтобы
  прокси мог стримить длинные тела; поиск добавляет к каждому запросу общий
  таймаут 60 с, как раньше.
- `GET /msx/` проксирует `http://tsmsx.yourok.ru`; `GET /msx` — gin-редирект
  `301` на `/msx/` с HTML-телом Go `http.Redirect`.
- `GET/POST /msx/start.json`: документ запуска (`logo.png` на схеме и
  `Host` запроса) и параметр, который живёт только в памяти, как у эталона.
- `GET /msx/trn?hash=` — есть ли хеш среди сохранённых; `POST /msx/trn` —
  метка плеера для `?hash=` или данные действия для `{"data":"…:<hash>"}` с
  `live`-блоком для загруженного торрента. Ошибки тела — тексты Go
  `encoding/json` (`go_decode`: сканер Go для первого значения, остальное
  игнорируется); ответ `400` уходит с `text/plain`, потому что `BindJSON` уже
  записал статус. Невалидный хеш — `500`, как паника `NewHashFromHex` у
  эталона.
- `/msx/proxy?url=…&header=Имя:значение` — любой метод; тело уходит потоком
  (chunked, как у Go), редиректы `301/302/303` превращают запрос в `GET`;
  клиенту возвращаются только статус, `Content-Length` и `Content-Type`.
- `/msx/imdb/:id` — редирект `301` на постер из подсказок IMDb, `404` без
  постера, JSON как есть для `id` с `.json`.
- `GET/POST /files` — символическая ссылка `<data-dir>/media`; `/files/*` —
  модуль `file_server`, повторяющий gin `StaticFS` поверх Go
  `http.FileServer`: пустой `404` для отсутствующего пути, относительные
  редиректы канонизации, HTML-листинг, `ServeContent` с условными запросами,
  диапазонами и multipart.

Ограничения:
- `GetTorrent` эталона в `trn` активирует сохранённый, но не загруженный
  торрент; `TorrentCommand::Get` Rustorr его не загружает, поэтому метка
  появляется только после активации другим путём (например, `/stream`).
- `/logo.png` из документа запуска относится к старому UI и не отдаётся до
  R8.
- Прокси не добавляет `User-Agent: Go-http-client/1.1` и
  `Accept-Encoding: gzip`, поэтому сжатый ответ сайта эталон отдаёт
  распакованным без `Content-Length`, а Rustorr — как есть (сайт без запроса
  сжатия обычно и не сжимает).
- Тип содержимого `/files/*` определяется `mime_guess` и упрощённым
  `DetectContentType`; у эталона — таблица Go и системный `mime.types`.
- gin перенаправляет любой маршрут с лишним или недостающим `/`; Rustorr
  повторяет это только для `/msx`.

Доказательства: `/tmp/rustorr-contract/r7-r74a/` — оба снимка `r7`
действительны (86 случаев); все 44 сценария MSX и `/files` совпадают,
включая исходящие запросы прокси; остаются 11 core-различий модулей
R7.5–R7.8. `tools/r4.sh check` — 236 тестов. Проба `deferred-msx` профиля
`direct` стала core-сценарием `msx-start` и ушла из allowlist; регрессия
`direct` (`/tmp/rustorr-contract/r7-r74-direct/`) — 46 случаев, 0
core-различий.

## R7.4 — Bonjour и DLNA

- Новый крейт `rustorr-discovery` (без зависимостей от других крейтов
  workspace): интерфейсы (`getifaddrs`), имена и идентификаторы, собственный
  mDNS-ответчик и SSDP. HTTP-часть DLNA — модуль `rustorr_http::dlna` на
  отдельном слушателе; запуск и остановку оркестрирует композиционный корень
  (`rustorr-server/src/discovery.rs`) через порт `Discovery` HTTP-слоя.
- Жизненный цикл как у эталона: при старте — по сохранённым настройкам;
  `settings set` перезапускает DLNA и Bonjour по флагам *из запроса* (имена
  — из действующих настроек); `settings def` останавливает оба, хотя
  по умолчанию Bonjour включён; `add`/`rem`/`wipe` перезапускают DLNA, если
  в сохранённых настройках включён `EnableDLNA`.
- Bonjour повторяет `grandcat/zeroconf` `RegisterProxy`: `_torrserver._tcp`
  и `_http._tcp`, по ответчику на службу; две пробы и два объявления (через
  1 и 2 с) на каждом интерфейсе, прощальные записи с TTL 0 при остановке;
  TTL 3200, адреса — 120; cache-flush у SRV/TXT всегда, у адресов — только в
  объявлениях; ответы на запрос — на интерфейсе запроса, на QU — unicast;
  подавление известных ответов для PTR. Имена из пакета разбираются с
  экранированием `miekg/dns`, поэтому, как у эталона, экземпляр с пробелом в
  имени не отвечает на прямой запрос, а A-запросы хоста остаются без ответа.
  Интерфейсы и имена — правила `server/bonjour` (исключения `docker`,
  `veth`, `br-`…, без loopback и link-local).
- SSDP повторяет `dms/ssdp`: сервер на каждом up/multicast интерфейсе,
  `ssdp:alive` для шести типов на каждом адресе раз в 30 с (`max-age=75`),
  ответы на `M-SEARCH` со случайной задержкой в пределах `MX` (1–10, разбор
  `MX` как `ParseUint(s, 0, 0)`), `ssdp:byebye` при остановке; заголовки
  ответа — в порядке и регистре Go `http.Response.Write`.
- HTTP DLNA (порт 9080 и выше, первый свободный): `/rootDesc.xml` байт в
  байт как `MarshalIndent` dms, SCPD-документы dms (BSD, лицензия рядом) через
  `ServeContent` со временем старта процесса, SOAP `/ctl` — три службы,
  ContentDirectory TorrServer (`Torrents` → категории → торренты → медиафайлы
  по таблице `server/mimetype`, ссылки `/play` на `Host` запроса и веб-порт),
  ошибки UPnP и тексты `encoding/xml`; ответы длиннее 2 КиБ идут chunked, как
  у Go; `SUBSCRIBE` с первым событием через 100 мс на `CALLBACK`; UUID —
  MD5 от FriendlyName; имя по умолчанию — из GECOS пользователя и имени хоста.

Ограничения:
- Иконки устройства — свои (48×48 и 120×120 PNG), не TorrServer'а
  (allowlist `r7-dlna-icon-*`).
- `/subtitle` всегда `404`, `/debug/pprof/` — корневая страница: dms отдаёт
  `<path>.srt` относительно рабочего каталога и профилировщик Go, Rustorr
  локальные файлы по DLNA не открывает.
- IPv6 mDNS (`ff02::fb`) не объявляется; AAAA-записи адресов публикуются
  через IPv4.
- Дата `dc:date` — в UTC (у эталона — в локальной зоне процесса; в образах
  обеих целей это UTC).
- Имя DLNA по умолчанию зависит от пользователя процесса: эталон в
  контейнере работает от root, Rustorr — от `rustorr` без GECOS
  (allowlist `r7-dlna-root-desc-default-name`).
- Перезапуск DLNA ждёт завершения активных запросов не дольше секунды;
  dms закрывает только слушатель, и старые соединения живут дальше.

Стенд: сеть `discovery` (internal macvlan) для multicast, пробы mDNS/SSDP в
сервисе-фикстуре, флаг `expect_connection_refused` (см.
`tools/contract/README.md`). Сценарии mDNS-запросов ждут 3,5 с после
`settings set`: у эталона `set` длится ~3,5 с (после Bonjour он
перезапускает Rutor), и обе серии объявлений заканчиваются до ответа, а
Rustorr отвечает сразу — без паузы проба ловила бы его объявления.

Доказательства: `/tmp/rustorr-contract/r7-r74c/` — оба снимка `r7`
действительны (152 случая); все 11 сценариев Bonjour и 8 SSDP совпадают, из
47 сценариев DLNA — 43, а четыре (три иконки и имя по умолчанию) вместе с
`r7-root` и `r7-stat` точно совпадают с allowlist; остаются 11
core-различий модулей R7.5–R7.8. Регрессия `direct` (`/tmp/rustorr-contract/r7-r74c-direct/`) —
46 случаев, 0 core-различий. `tools/r4.sh check` — 272 теста.

## R7.5 — WebDAV и FUSE

- Новый крейт `rustorr-vfs` — `server/torrfs` поверх `ClientCore`:
  категории (`SanitizeName`, пустая — `other`) → торренты (название или хеш;
  при `ShowFSActiveTorr` только с метаданными) → файлы по display path
  anacrolix. Чтение каталога торрента без метаданных загружает его, ожидая до
  `TorrentDisconnectTimeout × 2` попыток по 0,5 с. Режимы `0555`/`0444`,
  размер каталогов 4096, фиксированные времена корня и категорий, путь ниже
  файла открывает файл, `..` и пустые элементы — `ErrInvalid`. Дети
  сортируются по имени (у эталона — порядок Go map).
- `rustorr_http::serve_content` — Go `http.ServeContent` для памяти, файла и
  торрента: предусловия с ETag (`If-Match`, `If-None-Match`, `If-Range`) и
  датами, диапазоны, multipart, тип по расширению или сниффинг. Раздача
  `/files/*` и DLNA переведены на него; таблица типов TorrServer
  (`media_type`) общая, потому что `server/mimetype` регистрирует её в Go
  глобально.
- WebDAV (`--webdav` / `RUSTORR_WEBDAV`, `/dav`, без HTTP-авторизации, как у
  эталона) — порт `x/net/webdav` поверх read-only ФС: `OPTIONS` (`Allow` по
  типу ресурса), `GET/HEAD/POST` через `ServeContent` с ETag
  `"%x%x"` (mtime в нс, размер), `PROPFIND` (allprop/propname/prop/include,
  глубины 0/1/infinity, живые свойства, `getcontenttype` по расширению или по
  первым байтам торрента), `PROPPATCH` (403 для живых свойств, 500 для
  мёртвых), `LOCK`/`UNLOCK` (`memLS`: токены от времени старта, таймауты,
  `If`), запись и копирование — статусы read-only ФС эталона. Ответы
  multistatus длиннее 2 КиБ идут chunked; методы вне маршрутов gin — `404`.
- FUSE (`--fuse-path` / `RUSTORR_FUSE_PATH`; крейт `rustorr-fuse` на `fuser`
  за cargo-feature `fuse`, включённой по умолчанию): монтирование `mount(2)`
  с типом `fuse.torrserver`, источником `torrserver-fuse`, `nosuid,nodev`,
  `allow_other,max_read=131072` (без права монтировать — `fusermount3`);
  атрибуты `go-fuse` (nlink 0, inode 0 у корня и `2^63+n` у остальных, блоки
  по 512 байт), `DIRECT_IO`, чтение через playback с переиспользованием
  потока при последовательном чтении; ответы `go-fuse` для
  нереализованного: создание и `setattr` — `EROFS`, `mkdir`/`rename` —
  `ENOTSUP`, `unlink`/`rmdir` — успех без удаления. Ошибка монтирования
  завершает процесс, при остановке ФС размонтируется.

Ограничения:
- FUSE в cargo-feature по умолчанию, а не в отдельном образе, как планировал
  R7: `fuser` — чистый Rust без libfuse, образу пакеты не нужны. Монтирование
  требует `/dev/fuse` и root с `CAP_SYS_ADMIN` (в образе нет `fusermount3`).
- Порядок категорий, торрентов и файлов стабилен (по имени); у эталона он
  меняется от запроса к запросу. При двух торрентах с одинаковым именем в
  категории путь ведёт к первому по нашему порядку.
- `DetectContentType` — упрощённый набор сигнатур.
- WebDAV без авторизации повторяет эталон; доступ ограничивают WAF и сеть.

Стенд: каноническая форма WebDAV-XML в `diff.py` (сортировка responses и
свойств, даты и ETag торрентов, lock-токены),
правила заголовков `webdav-etag` и `lock-token: ignore`; FUSE снимается
отдельной командой `tools/r2.sh fuse-probe {reference|candidate}`
(`docker-compose.r7-fuse*.yml`, `tools/contract/r7/fuse_probe.sh`).

Доказательства: `/tmp/rustorr-contract/r7-r75/` — оба снимка `r7`
действительны (188 случаев); все 39 сценариев WebDAV совпадают, отложенные
различия точно совпадают с allowlist; остаются 8 core-различий модулей
R7.6–R7.8 (MCP, ffprobe, GStreamer). FUSE —
`/tmp/rustorr-contract/r7-r75-fuse/{reference,candidate}-fuse.txt`
идентичны после нормализации. Регрессия `direct`
(`/tmp/rustorr-contract/r7-r75-direct/`) — 46 случаев, 0 core-различий.
`tools/r4.sh check` — 285 тестов.

## R7.6 — MCP отложен

Решение пользователя 2026-09-23 (см. `r7-plan.md`): MCP уходит за первую
версию вместе с Telegram-ботом. `/mcp` у Rustorr отвечает обычным `404`;
сценарии `r7-mcp-*` и проба `deferred-mcp` профиля `direct` — в allowlist
с этапом `post-R10`.

## R7.7 — ffprobe

- `GET /ffp/status` — `{"available": …}`: есть ли `ffprobe` в `PATH` или
  рядом с исполняемым файлом (порядок `init` эталона).
- `GET /ffp/:hash/:id` — `ffprobe -loglevel fatal -print_format json
  -show_format -show_streams -show_chapters` по своему же
  `http://127.0.0.1:<порт>/play/<hash>/<id>` с таймаутом минута. Ответ
  пересобирается по структурам `vansante/go-ffprobe` v2.3.1: только их поля,
  в их порядке, с нулевыми значениями Go для отсутствующих, `omitempty` где
  он есть, числа-строки (`start_time`, `duration` формата) в Go-формате
  float, `side_data_list` по типизированным структурам. Ошибки — тексты
  go-ffprobe (`error getting data: error running /usr/bin/ffprobe [stderr]
  exit status N`), без бинарника — `404 {"error":"ffprobe binary not
  found"}`.
- Попутно найдено расхождение R6 в `/play`: для однофайлового торрента
  эталон играет единственный файл при любом индексе, а нечисловой индекс
  многофайлового — `400`. Исправлено. Тип содержимого `/play` и `/stream`
  теперь из общей таблицы `media_type` (для `.wav` — `audio/x-wav`, как у
  эталона).
- Образ: `Dockerfile` принимает `RUSTORR_RUNTIME_PACKAGES`; основной образ
  без ffmpeg (`available: false`), вариант с `ffmpeg` —
  `rustorr-r7-rustorr` в `docker-compose.r7-candidate.yml`.
- Стенд: торрент-фикстура `clip.wav` (две секунды синуса, генерируется на
  Python, хеши прежних фикстур не изменились) — первый настоящий медиафайл в
  раздаче; проверка здоровья сидера считает раздачи по `all.txt`.

Ограничения: несовпадение типа поля при разборе вывода ffprobe эталон
превращает в ошибку `json: cannot unmarshal …`, Rustorr берёт нулевое
значение.

Изоляция: `clip` остаётся в каталоге между сценариями (повторное
удаление и добавление вешает эталон с пиром в `pending`); очистка по
умолчанию снимает только его отметку «просмотрено», иначе после
переключения хранилища `viewedCount` расходится.

Доказательства: `/tmp/rustorr-contract/r7-r77d/` — оба снимка `r7`
действительны (193 случая); все сценарии `r7-ffp-*` и
`r7-play-single-any-index` совпадают, отложенные различия (11) точно
совпадают с allowlist; единственное core-различие — `r7-gst-settings`
(R7.8). Регрессия `direct` (`/tmp/rustorr-contract/r7-r77d-direct/`) —
46 случаев, 0 core-различий. `tools/r4.sh check` — 289 тестов.

## R7.8 — GStreamer

- Сборка без GStreamer (основной образ) отвечает как эталон без `-tags gst`:
  `GET /gst/settings` → `{"built_in":false}`, `POST` → `404
  {"error":"gstreamer is not built in"}`, других маршрутов `/gst` нет.
- Новый крейт `rustorr-gstreamer` — порт `server/gstreamer`. Всегда
  собираются и тестируются: конфиг (`Config`, нормализация, сохранённый
  документ), разбор вывода `gst-discoverer-1.0 -v`, таймлайн Matroska Cues
  (HTTP Range-чтения своего `/stream`), перепаковка фрагментов mp4mux в
  HLS-сегменты (`mp4box`, построчный порт с теми же текстами ошибок), разбор
  init-сегмента (кодеки, размер, диапазон), хранилище WebVTT-субтитров,
  плейлисты master/video/subs, сервис задач (кэш проб на час, заморозка
  неактивных задач после `InactiveMinutes` и удаление через 20 минут,
  `MaxTasks`). Раннер пайплайна на gstreamer-rs — за feature `runtime`: те же
  описания `gst_parse_launch`, флаги перемотки, pad-пробы
  (`videoStartProbe`, `videoSegmentClipProbe`), таймауты и перебор
  аппаратных кодеров.
- `rustorr-server` получает feature `gstreamer` (линкует libgstreamer);
  `rustorr-http` монтирует `/gst/*` только при переданном рантайме.
  `/gst/settings` — за HTTP-авторизацией, HLS-маршруты — без неё, как у
  эталона. Разбор `POST /gst/settings` повторяет `encoding/json`
  (регистронезависимые ключи, `null`, тексты `json: cannot unmarshal …`),
  `--rdb` → `403 read-only mode`. Настройки модуля хранятся в новой таблице
  SQLite `module_settings` (миграция V3), как у эталона — отдельным ключом
  `Settings/gstreamer`.
- `/torrents` `rem`, `drop` и `wipe` снимают HLS-задачу торрента.
- Попутно найдено расхождение R6: gin сопоставляет `/stream/*fname` и
  `/playlist/*fname` с пустым хвостом (`/stream/?link=…`, это URL источника
  GStreamer), axum — нет. Добавлены маршруты `/stream/` и `/playlist/` и
  сценарий `stream-empty-name` профиля `direct`.
- Образы: эталон `rustorr-r7-torrserver-gst` — тот же `Dockerfile.torrserver-r7`
  с `TORRSERVER_TAGS=gst` и `gstreamer1.0-plugins-base-apps`
  (`gst-discoverer-1.0`); кандидат `rustorr-r7-rustorr-gst` —
  `RUSTORR_FEATURES=gstreamer`, новый build-аргумент
  `RUSTORR_BUILD_PACKAGES` (dev-пакеты целевой архитектуры через multiarch и
  `PKG_CONFIG_PATH`) и пакеты GStreamer в `RUSTORR_RUNTIME_PACKAGES`.
  Dev-образ `tools/r4/Dockerfile.dev` несёт dev-пакеты GStreamer, и
  `tools/r4.sh check` линтует сборку с `gstreamer` и тестирует `runtime`.
- Стенд: профиль `r7-gst` (`docker-compose.r7-gst.yml`,
  `docker-compose.r7-gst-candidate.yml`), фикстура `movie.mkv` —
  8 секунд H.264 из I_PCM-макроблоков, PCM-звук, субтитры UTF-8 и Cues,
  собирается на Python (`tools/baseline/fixture_media.py`) без кодеков,
  хеши прежних фикстур не изменились. `diff.py` обнуляет время
  создания/изменения в `mvhd`/`tkhd`/`mdhd` ответов `video/mp4`: mp4mux
  пишет туда текущее время, и init-сегменты самого эталона различаются
  между запусками.

Ограничения:
- Рантайм линкуется, а не подгружается через `dlopen`, как у эталона
  (purego): бинарник с `gstreamer` без библиотек не запустится, поэтому это
  отдельный вариант образа. `found` в `/gst/echo` поэтому всегда `true`.
- Шина пайплайна опрашивается синхронно при чтении appsink, а не отдельной
  горутиной: ошибка простаивающего пайплайна замечается при следующем
  запросе, а не сразу.
- Пробы и создание задач сериализованы одним замком на сервис (у эталона —
  singleflight по ключу).
- `GST_REGISTRY` задаётся окружением образа (Rust без `unsafe` не меняет
  окружение процесса); эталон выставляет его сам.
- Встроенный Windows-рантайм эталона (`embedded_runtime`) не переносится:
  всегда `{"found":false,…}`, как у Linux-сборки эталона.

Кэш эталона (решение пользователя 2026-09-24): `tools/r2.sh capture
reference` переиспользует действительный снимок по ключу из аргументов,
исходников стенда, compose-конфигурации эталона и ID его образов (см.
`tools/contract/README.md`); `tools/r2.sh compare <run> --profile …` снимает
обе цели и пишет `diff.json`. Любая правка `scenarios.json` меняет ключ всех
профилей.

Стенд, мелочи: `r7-magnets-order` получил паузу 1,1 с между добавлениями
(время добавления у эталона секундное, при равенстве порядок случаен);
глобальная подготовка сбрасывает «просмотрено» для `clip` и `movie` — том
состояния кандидата общий у профилей `r7` и `r7-gst`.

Доказательства: `/tmp/rustorr-contract/r78-final-gst/` — оба снимка
`r7-gst` действительны (36 случаев), 0 core-различий: HLS-сегменты (в том
числе после перемотки и с Range), плейлисты, WebVTT и init-сегменты (после
нормализации времени mp4mux) совпадают байт в байт. `r7`
(`/tmp/rustorr-contract/r78-final-r7/`) — 196 случаев, 0 core-различий,
отложенные различия (11) точно совпадают с allowlist. Регрессия `direct`
(`/tmp/rustorr-contract/r78-final3-direct/`, после исправления `&stat`) —
47 случаев, 0 core-различий. `tools/r4.sh check` — 351 тест.

Попутно в `direct`: `/stream?…&stat` отдавал снимок ответа на добавление
(`stat: 0`), а эталон — `tor.Status()` уже загруженного торрента; теперь
текущий вид торрента.

## Следующая сессия

Состояние на 2026-09-24: ветка `r7-search`, R7.0–R7.8 выполнены (R7.6 MCP
отложен). Последние доказательства — в разделе R7.8 (каталог `/tmp` между
перезагрузками не сохраняется; при сомнениях переснять).

Порядок работ:

1. **R7.9 — оставшиеся флаги:** `--ip` (многократно), `--logpath`,
   `--weblogpath`, `--dontkill`, `--pubipv4`/`--pubipv6`, `--torrentaddr`,
   `--proxyurl`/`--proxymode` (у эталона без `--proxymode` при заданном
   `--proxyurl` режим `tracker`, неизвестный режим тоже сводится к
   `tracker`).
2. **Закрытие R7** (см. критерий выхода в `r7-plan.md`):
   - матрица возможностей по всем модулям (реализация, тест, статус amd64 и
     arm64, зависимости, ограничения); для GStreamer — проверить
     кросс-сборку варианта `gstreamer` под amd64 (`RUSTORR_BUILD_PACKAGES`
     через multiarch);
   - allowlist: убрать устаревшие пробы профиля `direct` к несуществующим
     путям (`deferred-storage`, `deferred-tmdb`, `deferred-ffprobe`,
     `deferred-webdav`, `deferred-dlna`, `deferred-gstreamer`) или перевести
     их в обычные сценарии; пересмотреть этап у `r7-stat` и
     `r7-dlna-root-desc-default-name` (сейчас `R7`): оставить осознанным
     различием с обоснованием или закрыть;
   - «Открытые пробелы R7.0» выше частично устарели (медиафикстуры для
     ffprobe и GStreamer есть, SSDP/mDNS-пробы сделаны); обновить.

Особенности стенда, которые легко забыть:
- `cargo` на хосте нет, только `tools/r4.sh cargo …`; полная проверка —
  `tools/r4.sh check` (сейчас 351 тест, включая сборку с `gstreamer`).
- `tools/r2.sh capture candidate` пересобирает образ кандидата профиля
  (`rustorr-r7-rustorr` с `ffmpeg`, `rustorr-r7-rustorr-gst` с GStreamer).
  Образ эталона `rustorr-r7-torrserver-gst` собирается вручную (см.
  «Команды»).
- Торренты `clip` и `movie` не удаляются между сценариями: повторные
  `rem`/`add` вешают эталон с пиром в `pending`, и снимок эталона становится
  недействительным.
- Снимки `r7` и `r7-gst` останавливают вторую цель; DLNA и Bonjour идут
  через internal macvlan-сеть `discovery`.

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

# GStreamer (R7.8)
docker compose -f docker-compose.baseline.yml -f docker-compose.r2-hermetic.yml \
  -f docker-compose.r7-capability.yml -f docker-compose.r7-gst.yml build torrserver
tools/r2.sh compare r7-gst-run --profile r7-gst

# регрессия R6
RUSTORR_CONTRACT_RUN_ID=r7-run-direct tools/r2.sh capture reference --profile direct
RUSTORR_CONTRACT_RUN_ID=r7-run-direct tools/r2.sh capture candidate --profile direct
tools/r2.sh diff /tmp/rustorr-contract/r7-run-direct/reference.json \
  /tmp/rustorr-contract/r7-run-direct/candidate.json \
  /tmp/rustorr-contract/r7-run-direct/diff.json

# FUSE
RUSTORR_CONTRACT_RUN_ID=r7-run-fuse tools/r2.sh fuse-probe reference
RUSTORR_CONTRACT_RUN_ID=r7-run-fuse tools/r2.sh fuse-probe candidate

# отдельные сценарии
RUSTORR_CONTRACT_RUN_ID=r7-one tools/r2.sh capture candidate --profile r7 \
  --only r7-ffp-clip,r7-ffp-bad-index
```
