# Стенд baseline R1

Стенд измеряет TorrServer MatriX.145 в Linux-контейнерах и воспроизводит те же
ID сценариев против образа Rustorr R5. Эталонный результат остаётся
baseline; overlay Rustorr добавляет управление детерминированным вытеснением и
перезапуском, не меняя ID эталонных нагрузок.

## Требования

- Docker Desktop или Linux-демон Docker
- Docker Compose v2
- Python 3.9+ на хосте для клиента измерений

Образ TorrServer при сборке выполняет checkout commit
`2c7fa43b9ac64a9eda27314c0b6791518497f188`.

## Команды

```sh
tools/baseline/r1.sh doctor
tools/baseline/r1.sh config
tools/baseline/r1.sh build
tools/baseline/r1.sh up
tools/baseline/r1.sh run
tools/baseline/r1.sh logs torrserver
tools/baseline/r1.sh down
```

Release-цель Rustorr и её проба перезапуска запускаются так:

```sh
RUSTORR_R1_TARGET=rustorr tools/baseline/r1.sh build
RUSTORR_R1_TARGET=rustorr tools/baseline/r1.sh run
RUSTORR_R1_TARGET=rustorr tools/baseline/r1.sh restart-probe
RUSTORR_R1_TARGET=rustorr tools/baseline/r1.sh down
```

Для диагностики хвоста потерь R5 запустите один и тот же фокусный цикл тёплых
Range-запросов по разу на каждую цель. Он записывает по каждому замеру дельты
`tc -s qdisc` и снимки TCP `ss -tin` из сетевого namespace цели; это
диагностическое доказательство, а не замена гейта R1.

```sh
tools/baseline/r1.sh netem-diagnose
RUSTORR_R1_TARGET=rustorr tools/baseline/r1.sh netem-diagnose
```

Результаты пишутся в `${RUSTORR_BASELINE_RUN_ROOT:-/tmp/rustorr-baseline}`.
Они намеренно лежат вне репозитория, потому что фикстуры, логи и сырая
телеметрия — сгенерированные данные.

## Формат результатов

`measurement.json` использует схему `rustorr.r1.measurement.v3` и хранит
сырые события проб вместе с p50, p95, средним и счётчиками сбоев. Он атомарно
перезаписывается после каждого сценария и после прерывания или сбоя сохраняет
`complete=false`, `last_scenario` и `stop_reason`. `docker-stats.json` и
`docker-stats-after.json` охватывают счётчики CPU, памяти, блочного I/O и
сети целевого контейнера до и после. `scenarios.json` — закоммиченная матрица
нагрузок.

У каждого Range-запроса лимит curl в 30 секунд реального времени, и он
принимается только при HTTP 206, точном `Content-Range`, точной длине в байтах
и детерминированном дайджесте фикстуры. Netem запускается в
короткоживущем tooling-контейнере, разделяющем сетевой namespace цели; уход
пира останавливает сидер между пробами. Их сырые события включают все
результаты управления. Завершённый прогон R1 должен выполнить каждую строку
матрицы и добавить полученные сырые события и агрегаты, прежде чем этап будет
отмечен как done.

Агрегация нескольких прогонов:

```sh
python3 tools/baseline/aggregate.py \
  --output /tmp/rustorr-baseline/baseline.json \
  /tmp/rustorr-baseline/<run>/measurement.json ...
```

Передача `--targets docs/benchmark-baseline.json` дополнительно оценивает
гейт R5 и отклоняет неполные входные данные. Гейт требует двух полных
прогонов, всех ID сценариев, отсутствия ошибок запросов/целостности/
предусловий/управления, утверждённых порогов cold/warm/seek-loaded/netem,
успешного ухода пира и ограниченных результатов seek-missing/seek-evicted.
Задержка детерминированного вытеснения сообщается без придуманного порога
паритета.

`netem-delay-loss` сохраняет ID сценария R1 и сетевую модель 80 ms / 1%, но
собирает 20 Range-замеров в каждом прогоне. Его эталонный порог — максимум из
двух значений p95 по прогонам зафиксированного TorrServer; кандидат проходит,
только если каждое из двух его значений p95 по прогонам укладывается в тот же
порог.

Агрегат записывает прокси транспортных остановок отдельно от ребуферизации
плеера; его нельзя выдавать за остановки декодированного воспроизведения.

Проверенный агрегат закоммичен как
[`docs/benchmark-baseline.json`](../../docs/benchmark-baseline.json). Каталоги
сырых прогонов остаются вне репозитория, и этот артефакт ссылается на них по
пути.
