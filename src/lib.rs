//! ccwatch — живой монитор сессий Claude Code (CLI и десктоп-приложение).
//!
//! Показывает запущенные сессии, шаги плана со шкалой прогресса, текущее действие,
//! субагентов с их моделями и временем работы и потраченные токены по моделям. Читает:
//!   ~/.claude/sessions/<pid>.json           реестр живых сессий и их статус
//!   ~/.claude/projects/<проект>/<id>.jsonl  транскрипт: usage, вызовы плана, инструменты
//!   ~/.claude/projects/<проект>/<id>/subagents/*.jsonl  транскрипты субагентов
//! План берётся из update_plan / TodoWrite / TaskCreate или из строк «ccwatch: план» и
//! «ccwatch: ✓2 ▶3» в ответах агента; оценки времени — из «~15м» в конце шагов.
//! Пока монитор открыт, он просит план у работающих сессий без плана: кладёт запрос в
//! ~/.ccwatch/ask/, сессии его передаёт хук (`ccwatch hook`). При выходе недоставленные запросы убираются.
//!
//! Где что лежит:
//!   app        — состояние монитора и обход сессий раз в секунду
//!   transcript — чтение транскриптов: токены, вызовы инструментов, время работы
//!   session    — сессия: план, субагенты, статус
//!   tips       — советы по экономии токенов и скорости, их эффект, история скиллов
//!   setup      — советы о самой программе: подключён ли хук ccwatch к Claude Code
//!   requests   — запросы к сессии через хук: план, советы, ваше сообщение, вопрос об идеях
//!   ideas      — идеи агента для анализатора и копилка ~/.ccwatch/ideas.jsonl по всем сессиям
//!   hook       — сам хук (`ccwatch hook`), который вызывает Claude Code
//!   fixes      — скрипты советов «для вас»: правка ~/.claude/settings.json
//!   procs      — где запущена сессия и сколько памяти у её процессов
//!   ui         — экраны и клавиши
//!   text, format, style, clock, json — общие мелочи: ширина строк, числа, цвета, время, поля JSON

pub mod app;
pub mod cli;
pub mod clock;
pub mod fixes;
pub mod format;
pub mod hook;
pub mod ideas;
pub mod json;
pub mod paths;
pub mod pattern;
pub mod procs;
pub mod registry;
pub mod requests;
pub mod session;
pub mod setup;
pub mod style;
pub mod text;
pub mod tips;
pub mod transcript;
pub mod ui;
