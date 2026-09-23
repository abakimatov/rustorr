# Характеризация контракта R2

Раннер снимает HTTP-поверхность зафиксированного TorrServer в виде сырого
воспроизводимого корпуса. Он намеренно сохраняет статус, заголовки, байты тела
и декодированный JSON; команда diff игнорирует только заголовки, явно
перечисленные в манифесте.

```sh
tools/r2.sh doctor
tools/r2.sh up
tools/r2.sh candidate-up
tools/r2.sh proxy-up
tools/r2.sh tls-up
RUSTORR_CONTRACT_RUN_ID=r6-reference tools/r2.sh capture reference --profile direct
RUSTORR_CONTRACT_RUN_ID=r6-candidate tools/r2.sh capture candidate --profile direct
RUSTORR_CONTRACT_RUN_ID=r6-auth tools/r2.sh capture reference --profile auth --basic-auth contract:fixture
RUSTORR_CONTRACT_RUN_ID=r6-proxy tools/r2.sh capture reference --profile proxy
RUSTORR_CONTRACT_RUN_ID=r6-restart tools/r2.sh restart-matrix reference
tools/r2.sh diff /tmp/rustorr-contract/<run>/reference.json /tmp/rustorr-contract/<run>/candidate.json
tools/r2.sh candidate-down
tools/r2.sh down
tools/r2.sh proxy-down
tools/r2.sh tls-down
```

Сгенерированные снимки и отчёты пишутся вне репозитория в
`RUSTORR_CONTRACT_RUN_ROOT` (по умолчанию `/tmp/rustorr-contract`).

По умолчанию каждый снимок один раз пересоздаёт трекер и Transmission, ждёт
анонса всех торрентов-фикстур, а затем один раз пересоздаёт выбранную цель.
Эти сервисы остаются запущенными на весь корпус. Подготовка/очистка цели для
каждого сценария сбрасывает каталог, просмотренное и состояние живых сессий
без перезапуска пировой инфраструктуры; манифест использует готовность только
по метаданным, если сценарию действительно не нужны байты воспроизведения.
Задавайте `RUSTORR_RESET_SEEDER=0` только для фокусной диагностики против уже
проверенной фикстуры.

`docker-compose.r2-hermetic.yml` использует внутреннюю статическую сеть, так
что ни эталон, ни кандидат не могут обнаружить публичных пиров или публичный
адрес. `docker-compose.r6-contract.yml` добавляет Rustorr в эту сеть без
открытия порта на хосте. Клиент снимка запускается внутри tooling-образа и
обращается к целям как `http://torrserver:8090` или `http://rustorr:8090`.

Корпус действителен, только если у каждого запроса есть ненулевой HTTP-статус
и нет транспортной ошибки. `tools/contract/diff.py` также требует одинаковых
хешей манифеста, нуля core-различий и точного совпадения отложенных различий
с `tools/contract/deferred-routes.json`.

Необязательный proxy-overlay открывает nginx на `127.0.0.1:8091` (HTTP) и
`https://127.0.0.1:8443` (самоподписанный TLS). Он пробрасывает `Host`,
`X-Forwarded-Host`, `X-Forwarded-Proto` и `X-Forwarded-For` без буферизации
тел потоков. Профиль `tls-up` также запускает сам TorrServer с `--ssl` на
`https://127.0.0.1:8444`; для обоих локальных TLS-снимков используйте
`--insecure`.

## Профили

Сценарий без поля `profiles` входит только в `direct`; профили `auth` и
`proxy` снимают свои подмножества манифеста, и `diff.py` ожидает отложенные
различия allowlist только для сценариев, реально попавших в корпус.

- `direct` обращается к целям внутри сети фикстуры.
- `auth` пересоздаёт цель с `docker-compose.r6-auth.yml`: эталон получает
  флаг `--httpauth` (переменной окружения для auth у MatriX.145 нет), кандидат
  — `RUSTORR_HTTP_AUTH`; обе цели читают `tools/contract/accs.db`
  (`contract:fixture`).
- `proxy` идёт через настоящий nginx по TLS (`r2proxy`/`r6proxy`, порт
  `8443`, `--insecure` добавляется автоматически). Сценарий задаёт только
  `Host`; `X-Forwarded-Host`/`X-Forwarded-Proto` выставляет сам nginx. После
  пересоздания цели nginx перезапускается, потому что разрешает upstream один
  раз при старте.

Сценарии, которые строят абсолютные URL (`m3u-unicode`,
`playlist-individual`, `auth-share-playlist`), фиксируют `Host`, иначе имена
контейнеров `torrserver`/`rustorr` попадают в тело.

## Готовность

`wait_for_metadata` ждёт `stat >= 3`; с `"require_data": true` — ещё и
непустое `data` в `list`: эталон дописывает сгенерированное `data`
сохранённого торрента асинхронно, уже после `stat = 3`. `wait_for_torrent`
дополнительно читает `bytes=0-0` через `/play`; эталон держит такой запрос,
пока не подключится пир, поэтому проба, упавшая по таймауту, повторяется до
`--readiness-timeout`. Любой другой ответ пробы окончателен.

Эталон изредка оставляет пиры повторно добавленного торрента в `pending` на
минуты. Подготовка сценария — работа стенда, а не наблюдаемый запрос,
поэтому упавшая подготовка один раз очищается (teardown) и повторяется
целиком. Каждая попытка записана в `lifecycle` (`attempt`,
`setup-retry-cleanup` с причиной). Наблюдаемый запрос не повторяется
никогда: его сбой делает снимок недействительным.

## Матрица перезапуска

`tools/r2.sh restart-matrix {reference|candidate}` запускает
`restart_matrix.py`: несохранённый и сохранённый торрент, просмотренное для
обоих, изменённые настройки и WAF, затем перезапуск контейнера цели. Отчёт
`<run>/<target>-restart.json` содержит только стабильные поля каталога,
просмотренного, настроек и WAF до и после перезапуска, поэтому отчёты эталона
и кандидата сравниваются как JSON.

## Нормализация по наблюдению эталона

Каждое правило `normalization.json_paths` опирается на то, что эталон сам
возвращает разные значения для одного сценария. `/Filled` и
`/Pieces/*/Size` в `cache-get`: после чтения `bytes=0-65535` MatriX.145
обычно отдаёт `65536`, но иногда `81920` — упреждающее чтение anacrolix успело
принять ещё один 16-КиБ чанк (`r6-final2-direct` против `r6-final3-direct`,
сценарий не менялся). Остальные поля снимка кэша, включая набор кусков,
`Completed`, `Length` и `Readers`, сравниваются точно.

`normalization.json_strings` заменяет регулярным выражением динамические
фрагменты внутри строк JSON. Единственное правило — счётчики пиров в метках
MSX (`{ico:north} N / N {ico:south} N` в `POST /msx/trn`): это те же
`active_peers`/`total_peers`/`connected_seeders`, что уже нормализуются как
поля, только вшитые в текст. Цвет метки и остальная строка сравниваются
точно.

## Редиректы

`urllib` сам следует за редиректами. Шаг с `"follow_redirects": false`
получает сам ответ `3xx` (статус, `Location`, тело) — так наблюдаются
канонизирующие редиректы `/files/` и gin-редирект `/msx` → `/msx/`.

## Внешние сайты R7

Заглушка индексатора (`tools/contract/r7/fake_indexer.py`) заодно играет
внешний сайт для `/msx/proxy`: `/_fixture/echo` отвечает на любой метод тем,
что получил, `/_fixture/redirect` отдаёт `302` на `echo`,
`/_fixture/missing` — `404`. Журнал `/_requests` для этих путей хранит метод,
заголовки `X-*`, тело и способ его передачи (`framing`: `chunked`, `length`
или `none`). Каталог `tools/contract/r7/msx-media` смонтирован в обе цели как
`/srv/msx-media` и служит целью ссылки `POST /files`.
