# Матрица возможностей R7

Эталон — TorrServer MatriX.145 на `2c7fa43b9ac64a9eda27314c0b6791518497f188`.
Подробности реализации и ограничения каждого модуля — в соответствующем
разделе `r7-continuation.md`; решения пользователя — в `r7-plan.md`.

Колонки:
- **Тест** — чем подтверждено поведение: сценарии корпуса (профиль и число
  сценариев), внешние пробы, unit/HTTP/процессные тесты (`tools/r4.sh
  check`).
- **arm64** — стенд работает на arm64: корпуса, пробы и процессные тесты
  выполняются на нём.
- **amd64** — образ кросс-собирается из arm64 (`tools/r4.sh smoke`), запуск
  проверен под эмуляцией (`/echo`); корпуса на amd64 не прогонялись.

| Модуль | Реализация | Тест | arm64 | amd64 | Зависимости | Ограничения |
| --- | --- | --- | --- | --- | --- | --- |
| API веб-интерфейса (`/stat`, `/magnets`, `/download`, `/shutdown`) | `rustorr-http` (`web_api`), политика трекеров в `rustorr-lifecycle` | `r7` web-api (8), процессный тест `/shutdown` | корпус | сборка | нет | `/stat` — свой формат (allowlist, принятое различие); `/` — заглушка до R8; режимы трекеров 2/3 не убирают трекеры торрента у librqbit |
| Хранилище и TMDB (`/storage/settings`, `/tmdb/settings`) | `rustorr-http` (`settings_api`), `rustorr-state` | `r7` storage (8) | корпус | сборка | нет | источник истины — SQLite (ADR 0006), выбор JSON/BBolt только отражается |
| Режимы процесса (`--read-only`, `--max-stream-size`, `--torrents-dir`, `--search-without-auth`) | `rustorr-server`, `rustorr-lifecycle`, `rustorr-http` | unit и HTTP-тесты | тесты | сборка | нет | каталог торрентов опрашивается раз в секунду вместо fsnotify |
| Поиск Rutor | `rustorr-search` (`rutor`) | `r7` search (часть 12), герметичная база | корпус | сборка | база `rutor.ls` (скачивается) | — |
| Torznab | `rustorr-search` (`torznab`) | `r7` search (часть 12), фиктивный индексатор с журналом исходящих запросов | корпус | сборка | внешние индексаторы | — |
| MSX | `rustorr-http` (`msx_api`) | `r7` msx (44) | корпус | сборка | внешние URL `/msx/proxy` | — |
| Bonjour | `rustorr-discovery` | `r7` bonjour (11): mDNS-пробы в macvlan-сети | корпус | сборка | multicast (host network или macvlan) | IPv6 mDNS не объявляется |
| DLNA (SSDP, ContentDirectory) | `rustorr-discovery`, `rustorr-http` (`dlna`) | `r7` dlna (55): HTTP и SSDP-пробы | корпус | сборка | multicast | свои иконки (R8); `/subtitle` всегда `404`; имя по умолчанию зависит от пользователя процесса (allowlist, принятое различие стенда) |
| WebDAV | `rustorr-vfs`, `rustorr-http` (`webdav`) | `r7` webdav (39) | корпус | сборка | нет | порядок каталога стабилен (по имени), у эталона — порядок Go map |
| FUSE | `rustorr-fuse` (`fuser`, feature `fuse` по умолчанию) | `tools/r2.sh fuse-probe`: вывод идентичен после нормализации | проба | сборка | `/dev/fuse`, `CAP_SYS_ADMIN` | нет `fusermount3` в образе |
| MCP | — | `404`; `r7` mcp (5) и `deferred-mcp` в allowlist | — | — | — | отложено за первую версию (решение 2026-09-23) |
| ffprobe | `rustorr-http` (`ffprobe_api`) | `r7` ffprobe (6) | корпус | сборка | `ffprobe` (вариант образа с `ffmpeg`) | несовпадение типа поля ffprobe не превращается в ошибку |
| GStreamer | `rustorr-gstreamer`, `rustorr-http` (`gstreamer_api`); feature `gstreamer` | `r7` gstreamer (7, сборка без модуля), `r7-gst` (36): сегменты, плейлисты, субтитры, init побайтно | корпус | кросс-сборка варианта с `gstreamer`, `/gst/echo` под эмуляцией | GStreamer 1.x (линкуется), `gst-discoverer-1.0` | отдельный вариант бинарника и образа; шина опрашивается при запросах; пробы и задачи сериализованы |
| Несколько адресов (`--listen`, аналог `--ip`) | `rustorr-http` (`Listeners`), `rustorr-server` | процессный тест, unit | тесты | сборка | нет | — |
| Логи (`--log-file`, `--access-log-file`) | `rustorr-server` (`logging`), `rustorr-http` (`access_log`) | процессный тест; web-лог сверен с эталоном побайтно (без времени) | тесты | сборка | нет | время UTC; в файл идёт вывод `tracing`, не весь stderr |
| `--dont-kill`, сигналы | `rustorr-server` | процессные тесты (SIGHUP/SIGINT/SIGTERM, игнорирование) | тесты | сборка | нет | — |
| `--torrent-addr` | `rustorr-engine` (`listen_addr`) | unit | тесты | сборка | нет | — |
| Прокси BT-трафика (`--proxy-url`, `--proxy-mode`) | `rustorr-engine`, `rustorr-server` | unit, процессный тест отказа | тесты | сборка | SOCKS5-прокси | только `socks5://`, режимы `peers`/`full` (пиры и HTTP-трекеры вместе); `tracker` и прочие схемы — ошибка запуска (решение 2026-09-24) |
| Публичные адреса (`--public-ipv4/6`) | `rustorr-server` | unit | тесты | сборка | нет | проверяются и логируются, в анонсы не попадают (librqbit 9.0.1) |

Вне R7: Telegram-бот и MCP (после первой версии); старый UI (R8 — новый);
встроенный TLS, HTTPS-редирект, service/install и устаревшие алиасы CLI/env
(R9); вытеснение на уровне кусков.

Принятые различия в allowlist после закрытия R7 (этап `none`): `r7-stat` и
`r7-dlna-root-desc-default-name`. С этапом `R8` — корень и иконки DLNA, с
`R9` — `POST /shutdown`, с `post-R10` — MCP.
