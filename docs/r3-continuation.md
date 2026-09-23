# Состояние продолжения R3

Обновлено: `2026-09-21`

Это точка входа для продолжения после R3 в новой сессии.

## Текущий статус

R3 закрыт. Все три оставшихся gate пройдены, и
[ADR 0003](adr/0003-bittorrent-engine-selection.md) теперь `accepted`:
**adopt `librqbit 9.0.1`** за Rustorr-адаптером. Production Rustorr workspace,
HTTP routes и production cache ещё не реализованы — это работа R4.

Начальная точка: ветка `master`, `HEAD b816f5a`. В рабочем дереве есть
незакоммиченные R1–R3 изменения; коммит не создавался.

## Что закрыто в этой сессии

- **Peer exchange.** `tools/engine-spike/run.sh pex` поднимает трёхузловую
  топологию: seeder → `pex-middle` (знает tracker) → `pex-client`
  (trackers и DHT выключены на уровне session, LSD выключен, один initial
  peer). В двух прогонах клиент обнаружил 3 адреса, которых ему не давали, и
  скачал `8,126,464` байта / 31 piece с PEX-обнаруженного seeder против
  `262,144` байт с initial peer.
- **Negative control.** Тот же клиент без initial peer не нашёл ни одного
  пира и упал по deadline. Контроль вскрыл реальный дефект: `librqbit 9.0.1`
  игнорирует `AddTorrentOptions::disable_trackers`; работает только
  `SessionOptions::disable_trackers`. Первый «успешный» PEX-прогон был из-за
  этого невалиден и отброшен.
- **Cache eviction seam.** Новый probe `--evict-after-read` показал, что при
  удалении данных под живым торрентом движок молча отдаёт занулённые байты —
  ни ошибки, ни re-fetch. Отсюда [ADR 0004](adr/0004-cache-eviction-seam.md):
  кэш принадлежит Rustorr на storage-seam, а вытеснение — только на уровне
  торрента.
- **Metric mapping.** [engine-r1-metric-mapping.md](engine-r1-metric-mapping.md)
  фиксирует правило: engine-тайминги — нижняя граница полного пути Rustorr,
  поэтому ими можно только отклонить кандидата, но никогда не объявить parity.
- **Перемер по протоколу R1.** Первые «медленные» строки оказались сравнением
  разных величин. Seek с warm-up (как в R1) — `500.2 / 505.2 мс` против порога
  `4512.8 мс`; резолв метаданных магнита — `2855.9 мс` против `6058.5 мс`.
  Заодно найден дефект в самом probe: `initialized_ms` считался в конце и
  показывал полное время прогона; исправлено, добавлены `total_ms` и
  `warmup_read`.

## Прочитать сначала

- [`docs/adr/0003-bittorrent-engine-selection.md`](adr/0003-bittorrent-engine-selection.md)
  — решение `adopt` и пять условий, на которых оно принято.
- [`docs/adr/0004-cache-eviction-seam.md`](adr/0004-cache-eviction-seam.md)
  — политика кэша и вытеснения.
- [`docs/engine-r1-metric-mapping.md`](engine-r1-metric-mapping.md)
  — правило сравнения с R1-порогами.
- [`docs/engine-spike.md`](engine-spike.md) — harness, gates и runtime results.
- [`docs/implementation-plan.md`](implementation-plan.md) — общий план и
  append-only handoff history.

## Сырые артефакты

Вне репозитория, в `/tmp/rustorr-engine-spike/`:

- PEX: `pex-20260920T175048Z/`, `pex-20260920T175326Z/`;
- PEX control: `pex-control-20260920T174652Z/`;
- storage eviction: `20260920T175558Z/`, `20260920T175634Z/`;
- state-matched seek (протокол R1): `20260920T181105Z/`, `20260920T181129Z/`;
- резолв метаданных магнита: `20260920T181310Z/`;
- DHT bootstrap: `dht-two-peer-20260921T172000Z/client.json`;
- uTP-only: `20260920T171757Z/probe.json`;
- repeat matrix: `repeat-20260921T170700Z/`.

Отброшенные и невалидные прогоны перечислены в
[`engine-spike.md`](engine-spike.md); переиспользовать их нельзя.

## Проходящие проверки

- release-сборка `rustorr-engine-spike` в Docker;
- `cargo fmt --check`;
- `cargo clippy --locked --release -- -D warnings`;
- валидация конфигурации Docker Compose (baseline + r3-engine);
- `sh -n tools/engine-spike/run.sh`;
- `git diff --check`.

## Перенесено в R4 — не закрыто в R3

- Никаких HTTP Range или player-измерений для кандидата не существует; R1
  floors остаются непокрытыми.
- Два необъяснённых ~20-секундных случая: первое чтение после резолва магнита
  (`20597.9 мс` против `3.8 с` с `.torrent`) и torrent-scoped re-fetch
  (`~20.1 с`). Похоже на общую причину в переподключении к пирам после
  повторного добавления. R1-порога для них нет.
- Piece-level eviction невозможен без upstream-изменения — это единственный
  названный fork trigger.

## Точные следующие шаги

R3 и R4 закрыты. Следующий этап — R5: lifecycle/cache coordinator и прогон
`tools/baseline/r1.sh` против Rustorr. Актуальная точка входа —
[`r4-continuation.md`](r4-continuation.md); три перенесённых performance-риска
остаются за R5.

Этот файл исторический; новые handoff-записи ведутся в
[`implementation-plan.md`](implementation-plan.md) и
[`r4-continuation.md`](r4-continuation.md).
