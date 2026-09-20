# Контракты совместимости TorrServer

Эталон: MatriX.145, commit `2c7fa43b9ac64a9eda27314c0b6791518497f188`. Это исследование исходников, а не завершённая инвентаризация всех функций и не результат запуска. Пользователь требует работу существующих клиентов без изменений; список клиентских приложений и их особенностей выясняет разработка.

## Явные клиентские особенности

- Kodi: заголовок `Server: TorrServer (Portable SDK for UPnP devices)`. Также выставляются `Connection: close`, `transferMode.dlna.org: Streaming`, ETag и, по запросу, DLNA capabilities. [stream.go](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/torr/stream.go)
- ForkPlayer: вложенные плейлисты получают суффикс `&fn=file.m3u`; комментарий прямо связывает это с клиентским багом. VLC: директива `#EXTVLCOPT:input-slave=` перечисляет внешние аудиодорожки и субтитры. [m3u.go](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/web/api/m3u.go)

## Сравнительные сценарии

| Область | Что фиксировать у эталона и сравнивать |
| --- | --- |
| JSON API | Actions, обязательные поля, типы, отсутствие поля против null/0, пустой список `[]`, HTTP-коды, пустое тело против JSON ошибки |
| Torrent state | Числовые статусы 0–5, stat_string, integer-размеры, float-скорости, строковый bit_rate |
| Stream/play | Известный и новый торрент, автоматическое добавление, индексы файлов, комбинации save/preload/stat/m3u/fromlast, приоритет флагов, экранирование ссылок и имён |
| HTTP | GET/HEAD, обычные и suffix/open/multiple ranges, ошибочные диапазоны, ETag/conditional requests, байты тела, отмена чтения |
| M3U | MIME, attachment filename, абсолютные URL, порядок/индексы файлов, вложенный и объединённый формат, внешние дорожки |
| Состояние | Изменение Viewed, сохранение и рестарт, побочные эффекты HEAD, drop/rem/wipe |
| Доступ и размещение | Прямой доступ, BasicAuth, HTTPS reverse proxy, forwarded host/proto, CORS/OPTIONS, WAF |

Источники: [torrents.go](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/web/api/torrents.go), [state.go](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/torr/state/state.go), [stream API](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/web/api/stream.go), [play API](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/web/api/play.go).

Raw streaming делегирует диапазоны и conditional requests Go `http.ServeContent`; для Rust нужен фактический HTTP-контракт, а не совпадение названий методов. GET и HEAD используют один handler: по исходникам HEAD тоже создаёт reader и отмечает Viewed до подавления тела ответа. GST использует отдельную обработку диапазонов; его тесты отклоняют multipart range. Нельзя автоматически унифицировать поведение raw и GST endpoints. [Routes](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/web/api/route.go), [raw streaming](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/torr/stream.go), [GST tests](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/gstreamer/handlers_test.go).

## Модель доступа — часть совместимости

BasicAuth защищает управление, но уже известный торрент допускает воспроизведение через `/play` и `/stream?play` без credentials; индивидуальные плейлисты также не защищены так же, как общий playlist. Обязательная авторизация поверх каждого URL может нарушить передачу ссылки внешнему плееру. Сохранение модели TorrServer уже выбрано пользователем в Q11; нельзя незаметно ужесточить её в рамках оптимизации. [auth.go](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/web/auth/auth.go), [stream API](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/web/api/stream.go), [play API](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/web/api/play.go).

WAF берёт IP из `RemoteAddr`; за reverse proxy это может быть IP прокси. URL зависят от host/scheme, а размещение под URL-префиксом требует отдельной проверки. CORS и WAF также необходимо сравнить на реальном эталоне. [WAF](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/web/waf/waf.go), [location](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/utils/location.go), [MCP URLs](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/mcp/urls.go), [server.go](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/web/server.go).

## Предлагаемая организация проверки

1. Запустить Go-эталон и Rust на одинаковых временных данных с контролируемыми локальными seeder/tracker и single/multifile torrents; включить Unicode, пробелы, вложенные пути, внешние дорожки и файл больше кэша.
2. Выполнить одинаковые сценарии и сравнить status, headers, semantic JSON, body bytes и побочные эффекты. Нормализовать только заведомо динамические значения: время, скорости, peer counts и multipart boundary, сохранив сравнение структуры.
3. Использовать оригинальный веб-интерфейс как контрольного клиента без изменения его API-кода; дополнить playback-проверками доступных плееров и browser HLS. Новый интерфейс Rustorr тестировать отдельно.
4. Любое отличие классифицировать как регрессию совместимости либо отдельно согласованное изменение поведения. Прохождение API-тестов само по себе не доказывает работу всех неизвестных плееров.

Upstream-база: [JSON rejection tests](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/web/api/torrents_test.go), [MCP URL fixtures](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/mcp/urls_test.go), [MCP protocol tests](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/mcp/server_test.go), [GST tests](https://github.com/YouROK/TorrServer/blob/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/gstreamer/handlers_test.go), [Swagger](https://github.com/YouROK/TorrServer/tree/2c7fa43b9ac64a9eda27314c0b6791518497f188/server/docs).
