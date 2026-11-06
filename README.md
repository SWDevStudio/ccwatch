# ccwatch

Терминальный монитор сессий Claude Code: планы и оценки времени, токены, субагенты, история скиллов и советы по экономии.

**[Подробная инструкция →](https://swdevstudio.github.io/ccwatch/)** · [Локальная версия страницы](docs/index.html)

## Установка

Нужны Git и Rust с Cargo. На Windows для MSVC toolchain установите Visual Studio Build Tools с компонентами C++ и Windows SDK.

```sh
git clone https://github.com/SWDevStudio/ccwatch.git
cd ccwatch
cargo build --release
```

Windows / PowerShell: `.\target\release\ccwatch.exe` или `.\ccwatch.cmd`.

macOS / Linux: `./target/release/ccwatch`.

Для команды в PATH: `cargo install --path .` (каталог Cargo bin должен быть в PATH).

## Первый запуск

1. Запустите задачу в Claude Code, затем откройте ccwatch в другом терминале.
2. Выберите сессию стрелками и нажмите Enter.
3. Нажмите `s`, выберите совет о подключении хука ccwatch, нажмите Enter. Проверьте правку настроек и подтвердите `y`.
4. Проверьте подключение через `/hooks` в Claude Code; при необходимости перезапустите сессию.
5. Вернитесь в карточку и нажмите `p`. Агент получит запрос плана после вызова инструмента.

Хук сохраняет абсолютный путь к программе: подключайте его после выбора постоянного расположения бинарника. Node.js для Rust-версии и её хука не нужен.

## Команды и клавиши

```sh
ccwatch --help
ccwatch --once
ccwatch --session my-project --plan
ccwatch --session my-project --tips
ccwatch --session my-project --skills
ccwatch --bell
ccwatch --no-ask
```

В списке ↑/↓ выбирают сессию, Enter открывает карточку. В карточке Enter открывает план, `s` — советы, `i` — скиллы, `b` — идеи, `m` — сообщение, `p` — запрос плана. Esc возвращает назад, `q` выходит из монитора. `x` завершает CLI-сессию после подтверждения `y`; текущая работа прерывается. Возобновление: `claude --resume <ID>`.

Монитор читает `~/.claude/sessions/` и `~/.claude/projects/`, хранит свои данные в `~/.ccwatch/`. Другие пути задаются через `CLAUDE_CONFIG_DIR` и `CCWATCH_DIR`; окружение монитора и хука должно совпадать. Непустой `NO_COLOR` отключает цвет. Отдельный API-ключ для мониторинга не нужен.

## GitHub Pages

Workflow [.github/workflows/pages.yml](.github/workflows/pages.yml) публикует `docs/` при push в `master` и поддерживает ручной запуск. В GitHub включите **Settings → Pages → Build and deployment → Source → GitHub Actions**.

После успешного развёртывания адрес страницы: <https://swdevstudio.github.io/ccwatch/>. Статус публикации проверяйте во вкладке Actions. Без хостинга откройте `docs/index.html` в браузере.

Справка: [GitHub Pages workflows](https://docs.github.com/en/pages/getting-started-with-github-pages/using-custom-workflows-with-github-pages), [хуки Claude Code](https://code.claude.com/docs/en/hooks).
