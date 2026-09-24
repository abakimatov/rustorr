# Продолжение R8

Статус: `in progress` с 2026-09-24. План и решения пользователя — в
[`r8-plan.md`](r8-plan.md).

## R8.0 — прототип

Прототип ключевых экранов опубликован на согласование:
https://claude.ai/artifact/XRkycb7F1fTTy7nV3oe5WP (список торрентов, детали с
плеером, добавление и поиск, настройки; телефон — список и плеер; светлая и
тёмная темы). Стиль: тёплый нейтральный фон, акцент «ржавчина»
(`#B8481A` / `#E0703A`), Bricolage Grotesque для заголовков, IBM Plex Sans и
Mono для текста и технических значений. Ждёт отзыва пользователя.

## R8.1 — каркас

- `web/`: Vite 8, React 19, TypeScript 6 (strict), Tailwind 4 (токены темы в
  `src/styles.css`: светлая по умолчанию, тёмная по системе или
  `data-theme`), TanStack Query, react-i18next (русский и английский, язык
  по браузеру и выбору, тест на совпадение ключей), Vitest, ESLint
  (typescript-eslint strict, react-hooks). TypeScript 7 не взят: typescript-
  eslint поддерживает `<6.1`.
- Клиент API — `web/src/api/`: один `fetch` с `credentials: same-origin`
  (браузер сам добавляет Basic auth), ошибки — `HttpError` со статусом и
  телом.
- Встраивание: `crates/rustorr-http/build.rs` вшивает `web/dist` (или
  `RUSTORR_WEB_DIST`) через `include_bytes!`; без сборки остаётся прежняя
  заглушка корня, и Rust-проверки не требуют Node.
- Отдача (`rustorr-http/src/web_ui.rs`): `/` — `index.html` (`no-cache`),
  `/assets/*` — файлы с хешем в имени (`immutable`, год), файлы корня
  сборки — по своим путям. Всё за HTTP-авторизацией, кроме
  `site.webmanifest`, как у эталона; маршруты только `GET`; неизвестные
  пути по-прежнему получают `404 page not found`.
- `Dockerfile`: стадия `node:24-bookworm-slim` собирает интерфейс на
  платформе сборщика, `dist` копируется в стадию Rust.
- `tools/r4.sh web-check` (и первым шагом `check`): `npm ci` и `npm run
  check` (tsc, eslint, vitest, build) в `node:24`; `node_modules` в
  отдельном томе, чтобы не затирать хостовые нативные модули. `tools/r4.sh
  web <команда>` — любая команда в том же контейнере.

Проверка: `tools/r4.sh check` — 368 Rust-тестов и 5 тестов UI; образ
`docker build .` отдаёт интерфейс, манифест и ресурсы с нужными заголовками.

## Следующее

- Отзыв на прототип → шрифты (самостоятельная раздача, без Google Fonts),
  оболочка приложения (навигация, темы, язык).
- R8.2 — торренты по `r8-plan.md`.
