# Матрица совместимости R6

Эталон: TorrServer MatriX.145 на `2c7fa43b9ac64a9eda27314c0b6791518497f188`.

| Область | Классификация | Контракт | Статус пробы |
| --- | --- | --- | --- |
| Торренты/загрузка | required | все действия, побочные эффекты live/каталога, кодеки ссылок и multipart-загрузка | manifest |
| Настройки | required | 41 поле, значения по умолчанию, нормализация, сохранение и объявленные runtime-эффекты | manifest |
| Стриминг | required | GET/HEAD, Range, ETag, условные запросы, MIME и байты тела | manifest |
| Плейлисты | required | URL в M3U, порядок, имена файлов и директивы внешних плееров | manifest |
| Доступ | required | BasicAuth, CORS, WAF и поведение forwarded host/proto | manifest |
| Кэш/просмотренное | required | изменения состояния и побочные эффекты HEAD | manifest |
| Поиск/медиа | capability-specific | поиск, хранилище, TMDB, GStreamer и ffprobe | manifest |
| MCP | capability-specific | ответ initialize и ошибки протокола | manifest |
| DLNA/обнаружение | capability-specific | HTTP-проба DLNA и сервис Bonjour | manifest; Bonjour остаётся внешней пробой |
| WebDAV/FUSE | capability-specific | WebDAV OPTIONS и доступность маршрутов FUSE | manifest |
| Сервис/TLS | capability-specific | встроенный TLS, редиректы, маршруты service/install и устаревшие алиасы | allowlist R9 |

`manifest` означает, что запрос представлен в `scenarios.json`; runtime-статус
фиксируется только снимком корпуса. `404` от capability-specific эндпоинта
остаётся доказательством и не отфильтровывается из корпуса молча.

Для R6 `required` означает core-сценарий: различие в ответе проваливает
семантический diff. Различия capability-specific принимаются, только если их
сценарий есть в `deferred-routes.json` с владельцем, причиной и этапом
продолжения. Allowlist точный: объявленное ожидаемое различие, которое не
возникло, тоже проваливает diff.
