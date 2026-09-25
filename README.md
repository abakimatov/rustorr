# Rustorr

Сервер потокового воспроизведения торрентов на Rust, совместимый с
клиентами TorrServer MatriX.145: тот же HTTP API, плейлисты, DLNA и
Bonjour, поиск Rutor и Torznab, WebDAV и FUSE, HLS через GStreamer — и
новый веб-интерфейс на русском и английском.

Статус: release candidate `1.0.0-rc.1`. Совместимость проверяется
чёрным ящиком против закреплённого MatriX.145 (`tools/r2.sh`), скорость
старта и перемотки — стендом R1 (`tools/baseline/r1.sh`).

## Быстрый старт

На сервер с Docker, по SSH (образ собирается здесь, реестр не нужен):

```sh
tools/deploy.sh user@server deploy
```

Подробно — [`docs/deploy.md`](docs/deploy.md): HTTPS, reverse proxy,
обновление и откат, бэкапы, метрики, ресурсы и трафик.

Локально в Docker:

```sh
docker build -t rustorr .
docker run -d -p 8090:8090 -p 6881:6881 -p 6881:6881/udp \
  -e RUSTORR_PEER_PORT=6881 -v rustorr-data:/data rustorr
```

Интерфейс — `http://localhost:8090`. Клиенты TorrServer подключаются к тому
же адресу.

Бинарник из архива релиза (`rustorr-<версия>-<архитектура>.tar.gz`, Linux
glibc 2.36+):

```sh
./rustorr --data-dir /var/lib/rustorr --http-auth
```

Все параметры — `rustorr --help`; у каждого есть переменная `RUSTORR_*`.
Флаги MatriX (`-p`, `-d`, `--httpauth`, `--ssl`, …) и переменные `TS_*` его
Docker-образа тоже понимаются.

## Служебные команды

```sh
rustorr health                 # для проверок здоровья контейнера
rustorr backup state.tar.gz    # снимок состояния (можно на работающем)
rustorr restore state.tar.gz   # восстановление (сервер остановлен)
rustorr passwd admin           # пароль HTTP-учётной записи со stdin
```

## Разработка

Всё собирается и проверяется в контейнерах:

```sh
tools/r4.sh check        # Rust: fmt, clippy, тесты; UI: tsc, eslint, vitest, сборка
tools/r4.sh e2e          # сценарии Playwright на стенде
tools/r2.sh compare <run> --profile direct   # контрактный корпус против MatriX.145
tools/r9.sh smoke        # развёртывание на «чистый сервер» по SSH
tools/release.sh build   # артефакты релиза; verify — воспроизводимость
```

План и ход работ — [`docs/implementation-plan.md`](docs/implementation-plan.md).
