# Продолжение R6

Статус: `done` на 2026-09-23, commit `8a21044` в `master`.

Все критерии выхода R6 выполнены на финальном рабочем дереве: действительные
снимки direct/auth/proxy с одним манифестом, ноль core-различий, отложенные
различия в точности совпадают с allowlist, регрессия зелёная. Принятое
отклонение R5 `netem-delay-loss` остаётся открытым и не засчитано как
пройденный гейт. Реализация закоммичена в `master` как `8a21044`.

## Что реализовано

- `rustorr-lifecycle::ClientCore` отделяет HTTP от production-координатора;
  HTTP-тесты используют `InMemoryClientCore`.
- У живых сессий и каталога SQLite раздельная семантика `add`/`save`/`drop`/
  `rem`/`wipe`. Координатор соблюдает порядок: отменить и дождаться prefetch,
  проверить закрепления, удалить сессию движка, затем изменить кэш/SQLite.
- `settings set/def` после применения новых настроек выгружают все живые
  торренты по семантике `drop`, как MatriX.145 при переподключении клиента.
  Торрент с активным читателем остаётся загруженным — сознательное отклонение,
  см. [`r6-plan.md`](r6-plan.md).
- При старте координатор удаляет дисковый кэш торрентов, которых нет в
  каталоге: несохранённый торрент не переживает перезапуск, и его байты иначе
  лежали бы на диске вне мягкого лимита.
- Пути файлов многофайлового торрента начинаются с имени торрента, как у
  anacrolix в эталоне; это же меняет `data` по умолчанию и ETag.
- Порт движка предоставляет раннее создание сессии, разбор metainfo,
  метаданные/файлы, runtime-статус и создание читателя. Индекс файла в HTTP,
  начинающийся с единицы, отделён от индекса движка, начинающегося с нуля.
- Порт кэша поддерживает динамический мягкий лимит, снимок торрента,
  завершённые куски и диапазоны активных читателей. Клиентская потребность
  учитывается отдельно от физически загруженных байтов кусков.
- Поддержаны голый хеш, magnet, HTTP(S), file и `torrs://`; точный golden
  vector `torrs_hash` из MatriX.145 покрыт тестом.
- Маршруты R6 для torrents/upload, сырого stream/play, плейлистов, настроек,
  просмотренного, кэша и WAF, а также BasicAuth, CORS и доверенное
  forwarded-расположение.
- Сырое воспроизведение: GET/HEAD, полные/одиночные/открытые/суффиксные/
  multipart-диапазоны (boundary из 60 символов, как у Go), `416`, условные
  запросы, ETag/Last-Modified, MIME и заголовки Kodi/DLNA.
- Полный документ настроек из 41 поля и runtime-эффекты R6; схема v2
  сохраняет списки WAF, настройки, каталог и состояние просмотренного.

## Что изменено в стенде в этой сессии

- `capture` выбирает конфигурацию цели и точку входа по `--profile`: auth
  пересоздаёт цель с auth-overlay, proxy идёт через настоящий nginx по TLS.
- Эталон включает auth только флагом `--httpauth`; прежний overlay с
  `TS_HTTPAUTH` его не включал, поэтому ранние «auth»-снимки эталона
  недействительны как доказательство.
- Проба готовности пиров повторяется до `--readiness-timeout`; сценарии,
  которые обращаются к `/play`, ждут готовности пиров; `require_data` ждёт
  асинхронно дописанного эталоном `data`.
- Сценарии с абсолютными URL фиксируют `Host`; allowlist применяется только к
  сценариям, попавшим в профиль; у кандидата статический адрес `172.31.250.20`.
- Новый сценарий `settings-change-unloads-live` и матрица перезапуска
  `tools/r2.sh restart-matrix`.
- Нормализация `/Filled` и `/Pieces/*/Size` по наблюдению недетерминированности
  эталона (65536 против 81920 в неизменном `cache-get`).

Подробности — в [`tools/contract/README.md`](../tools/contract/README.md).

## Доказательства

Все снимки — манифест `c0ad53710b6d88f074062ceef69e1d229733e02bb99e627f1d5f63f121cad762`,
кандидат собран из финального дерева.

| Артефакт | Результат |
| --- | --- |
| `/tmp/rustorr-contract/r6-final5-direct/` | оба снимка действительны, 46 случаев; 0 core, 4 отложенных = allowlist (`deferred-download`, `deferred-mcp`, `deferred-msx`, `deferred-search`) |
| `/tmp/rustorr-contract/r6-final5-auth-retry/` | оба действительны, 5 случаев; 0 различий |
| `/tmp/rustorr-contract/r6-final5-proxy/` | оба действительны, 1 случай; 0 различий |
| `/tmp/rustorr-contract/r6-final5-restart/` | каталог, просмотренное, настройки и WAF до и после перезапуска совпадают с эталоном |
| `/tmp/rustorr-contract/r6-final5-auth/` | эталон недействителен (см. ниже); не использовать |

Нестабильность эталона: TorrServer иногда не подключается к единственному
сидеру — пиры остаются в `pending` (`total_peers 2, pending_peers 2,
active_peers 0`) дольше 90 с. За сессию это случилось в 5 снимках эталона из
26 (`r6-reference-current`, `r6-final2-direct`, `r6-final2-auth`,
первая попытка `r6-final4-auth`, `r6-final5-auth`), в изолированных повторах
auth — 0 из 4. У кандидата ни одного такого сбоя. Недействительный снимок
отбрасывается целиком и снимается заново; отбор отдельных ответов не
делается.

Регрессия на финальном дереве:

- `tools/r4.sh check`: fmt, clippy `-D warnings`, 189 тестов, границы крейтов;
  `cargo test -p rustorr-server --features r5-test-control` — зелёные.
- Release-сборка, cross-build `x86_64-unknown-linux-gnu`, Docker smoke
  (arm64 и amd64, чистая остановка, повторный старт со schema v2).
- Гейты R1 (`/tmp/rustorr-baseline/20260922T200358Z`,
  `/tmp/rustorr-baseline/20260922T200531Z`, агрегат
  `/tmp/rustorr-baseline/r6-aggregate.json`): ноль ошибок запросов,
  целостности, предусловий и управления; cold/warm/seek-loaded, peer-departure,
  seek-missing/evicted и детерминированное вытеснение пройдены.
  `netem_range_floor` провален: p95 `1527.734` и `1035.496 ms` при пороге
  `988.317 ms` — это принятое открытое отклонение R5.
- Проба перезапуска R5 с `save_to_db:true`
  (`/tmp/rustorr-baseline/20260922T200701Z-restart`): до и после рестарта HTTP
  206 и дайджест `31e67ed8…`, чтение без сидера после рестарта — `3.556 ms`.
- Python-компиляция, `test_methodology`, JSON, синтаксис shell, конфигурации
  Compose всех overlay и `git diff --check`.

## Открытое

- `netem-delay-loss` (R5) — по-прежнему не пройден; решение о транспорте
  остаётся за отдельной работой, R6 его не закрывает.
- Нестабильность подключения пиров у эталона — свойство стенда, не
  кандидата; если она начнёт мешать, следующий шаг — снять логи anacrolix в
  момент сбоя.

## Команды воспроизведения

```sh
tools/r4.sh check
tools/r4.sh cargo test -p rustorr-server --features r5-test-control --locked

tools/r2.sh up
tools/r2.sh candidate-up
for profile in direct proxy; do
  RUSTORR_CONTRACT_RUN_ID=r6-$profile tools/r2.sh capture reference --profile $profile
  RUSTORR_CONTRACT_RUN_ID=r6-$profile tools/r2.sh capture candidate --profile $profile
  tools/r2.sh diff /tmp/rustorr-contract/r6-$profile/reference.json \
    /tmp/rustorr-contract/r6-$profile/candidate.json \
    /tmp/rustorr-contract/r6-$profile/diff.json
done
RUSTORR_CONTRACT_RUN_ID=r6-auth tools/r2.sh capture reference --profile auth --basic-auth contract:fixture
RUSTORR_CONTRACT_RUN_ID=r6-auth tools/r2.sh capture candidate --profile auth --basic-auth contract:fixture
tools/r2.sh diff /tmp/rustorr-contract/r6-auth/reference.json \
  /tmp/rustorr-contract/r6-auth/candidate.json /tmp/rustorr-contract/r6-auth/diff.json
RUSTORR_CONTRACT_RUN_ID=r6-restart tools/r2.sh restart-matrix reference
RUSTORR_CONTRACT_RUN_ID=r6-restart tools/r2.sh restart-matrix candidate
```

Не сравнивать снимки с разными `manifest_sha256`. Не редактировать
`tools/r2.sh` во время снимка: `sh` дочитывает скрипт по ходу выполнения.
