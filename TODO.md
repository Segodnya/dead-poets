# TODO — dead-poets v1

Декомпозиция PLAN.md (Addendum 2 — авторитетный). Порядок фаз = порядок выполнения.
Каждая задача — один тестируемый юнит; строка `тест:` — критерий готовности.

## Фаза 0 — Scaffolding

- [ ] `cargo init` бинарный крейт, структура модулей (config, po, decode, guard, extract, liveness, report, cli)
  тест: `cargo build` зелёный, `cargo run -- --help` печатает usage.
- [ ] Добавить и **запинить** зависимости: clap(derive), serde, toml, poparser, tree-sitter (+php/js/ts), regex, ignore, rayon, dashmap, anyhow, colored, serde_json, log, env_logger
  тест: `cargo tree` показывает фиксированные версии tree-sitter без ABI-конфликтов; сборка зелёная.

## Фаза 1 — Config (dead-poets.toml)

- [ ] Структуры `[scan]`: `po_patterns`, `source_extensions`, `ignore_dirs`, `source_roots` (всегда `Vec`, даже для одного корня)
  тест: десериализация примера из PLAN → `source_roots == ["."]`; пропуск поля даёт дефолт.
- [ ] Модель вызова `[[calls]]`: `{ lang, kind: function|method|filter, name, receiver: Option<Vec<String>>, key_arg_index }`
  тест: парсинг трёх блоков из примера example_repo (php method+receiver, js function, twig filter) даёт 3 корректных записи.
- [ ] `[output]`: `mode`, `format` (text|json), `fail_on` (never|dead|dead-or-blind, дефолт `dead`); `min_guard_len` (дефолт 3)
  тест: дефолты применяются при отсутствии полей; неизвестное значение `fail_on` → ошибка конфигурации.
- [ ] `[whitelist]`: `file` (по строке на ключ) и/или инлайн `keys`
  тест: оба источника объединяются в один `HashSet`; отсутствие файла-вайтлиста — мягкая ошибка с понятным сообщением.

## Фаза 2 — PO index

- [ ] Сбор `.po`/`.pot` по `po_patterns` и парсинг через poparser
  тест: на фикстуре из 3 локалей собираются все файлы; битый PO → ошибка с путём.
- [ ] Построение универсума ключей как **объединения msgid по всем локалям**; модель ключа `(Option<msgctxt>, msgid, Option<msgid_plural>)`
  тест: ключ, присутствующий в 1 из 3 локалей, попадает в индекс; plural-ключ хранит обе формы.
- [ ] Пропуск obsolete (`#~`) и fuzzy записей
  тест: obsolete- и fuzzy-msgid отсутствуют в индексе, активные — присутствуют.
- [ ] Зафиксировать, что poparser отдаёт **декодированные** msgid
  тест: PO `msgid "a\nb"` приходит как строка с реальным переводом строки.

## Фаза 3 — Literal decoding (единая точка «литерал vs guard»)

- [ ] PHP single-quote `'…'`: разэкранировать только `\\` и `\'`
  тест: `'a\nb'` → буквально `a\nb` (без перевода строки); `'it\'s'` → `it's`.
- [ ] PHP double-quote `"…"`: полные эскейпы; при наличии `$`/`{$}` интерполяции — **не литерал**, маршрут в guard-путь
  тест: `"a\nb"` → реальный `\n`; `"role_$x"` помечается как нелитерал (идёт в guard, не в literal-set).
- [ ] JS обычные и template-строки; Twig строковые литералы → канонический рантайм-вид
  тест: JS `` `plain` `` → `plain`; Twig `'key'` → `key`; canonical-сравнение PHP `"a\nb"` == PO `a\nb`.

## Фаза 4 — Guard layer (центральный инвариант корректности)

- [ ] Извлечение **максимальных** статических фрагментов из нелитерального аргумента; позиция → `Prefix`/`Suffix`/`Contains`
  тест: `` `cf_subtype_${x}` `` → `Prefix("cf_subtype_")`; `` `${x}_delete_confirm_1` `` → `Suffix("_delete_confirm_1")`.
- [ ] Инвариант `min_guard_len`: фрагмент короче порога **не** создаёт guard; при отсутствии годных фрагментов → `blind_count++`, guard не эмитится
  тест: `` `${a}_${b}` `` (фрагмент `"_"` длиной 1) → 0 guard'ов, blind_count += 1; не помечает весь каталог Alive.
- [ ] Матчинг guard'а: prefix/suffix/contains против **декодированного** msgid
  тест: guard `Prefix("cf_subtype_")` матчит msgid `cf_subtype_foo`, не матчит `other_key`.

## Фаза 5 — Extractor adapters (per-kind matchers)

- [ ] Интерфейс адаптера `extract(source) -> { literals: Set, guards: Vec<Guard>, blind: usize }`; `key_arg_index` выбирает узел-аргумент → модуль декодирования (Фаза 3)
  тест: общий контракт — на пустом источнике возвращает пустые наборы и blind=0.
- [ ] `function`-матчер (PHP `function_call_expression`, JS/TS вызовы) по имени из config
  тест: `i18n('key')` → literal `key`; `notI18n('key')` игнорируется.
- [ ] `method`-матчер (`member_call`/`scoped_call`) по имени **и** receiver с нормализацией объекта (`i18n`, `$this->i18n`)
  тест: `$i18n->get('k')` и `$this->i18n->get('k')` → literal `k`; `$other->get('k')` игнорируется.
- [ ] `filter`-матчер Twig (regex по имени фильтра из config)
  тест: `{{ 'k'|i18n }}` → literal `k`; `{{ var|i18n }}` → blind_count += 1.
- [ ] Резолв конкатенации двух строковых литералов
  тест: PHP `'Hello, ' . 'World'` → literal `Hello, World`; `'Hello, ' . $x` → guard `Prefix("Hello, ")`.
- [ ] Подключение грамматик: PHP `language_php()` (HTML-aware, **не** `language_php_only()`), JS, TS, TSX
  тест: `.php` со смешанной HTML/Twig-разметкой парсится без паники; `.tsx`-файл обрабатывается грамматикой TSX.

## Фаза 6 — Liveness

- [ ] Классификация: ключ **Alive** при literal-match ИЛИ guard-match, иначе **Dead**; на живых — тег `alive_via: literal|guard`
  тест: literal-хит → Alive/literal; только guard-хит → Alive/guard; нет совпадений → Dead.
- [ ] Сводка blind per-language (агрегация `blind_count` из адаптеров)
  тест: 2 blind-сайта в JS + 1 в Twig → `{js:2, twig:1}`; никогда не скрывается.

## Фаза 7 — tree-sitter concurrency

- [ ] Параллелизм по файлам через `rayon`; thread-local `Parser` на язык (переиспользуется между файлами), сбор в `DashMap`
  тест: прогон на N файлах однопоточно и многопоточно даёт идентичный набор literals/guards/blind.

## Фаза 8 — Reporter

- [ ] Текстовый вывод: ранжированный список Dead (colored) + per-language blind-сводка + однострочный header-caveat про scope
  тест: на смешанной фикстуре печатает только Dead-ключи; header содержит предупреждение о внешних потребителях.
- [ ] JSON-вывод, зеркалящий бакеты Dead/Alive(alive_via) и blind-сводку
  тест: `--format json` → валидный JSON; число Dead совпадает с текстовым выводом.
- [ ] Exit-коды: `0` нет Dead, `1` есть Dead, `2` ошибка; управляется `fail_on`
  тест: каталог с Dead + `fail_on=dead` → код 1; `fail_on=never` → код 0; нет PO-файлов → код 2.

## Фаза 9 — CLI

- [ ] clap derive: сабкоманда `scan` с `path`, `--config`, `--format`, `--verbose`
  тест: `scan ./proj --format json -vv` парсится в корректную структуру; дефолты как в PLAN.

## Фаза 10 — Интеграция (testbed = example_repo)

- [ ] Поставить `examples/example_repo.toml` (как в PLAN: php method+receiver, js function, twig filter, po_patterns)
  тест: конфиг десериализуется без ошибок реальной структурой config.
- [ ] E2E на example_repo-shaped фикстуре: literal-хит, dynamic-guard-хит, blind-сайт, truly-dead ключ
  тест: `cf_subtype` (через `` i18n(`cf_${x}`) ``) → Alive/guard; `i18n($x)` → blind; неиспользуемый ключ → Dead; `cargo test` зелёный.
- [ ] Standalone-проверка: тот же бинарь на другом репозитории сменой config (без правок кода)
  тест: запуск с минимальным чужим config даёт корректный отчёт без хардкода example_repo.
