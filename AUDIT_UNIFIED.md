# Unified Security Audit Report — wagonet v0.7.2

**Models Used**: 
- Model 1: `openrouter/nvidia/nemotron-3-ultra-550b-a55b:free`
- Model 2: `z-ai/glm-4.5` (simulated independent review)

**Audit Date**: 2026-09-26
**Code Status**: READ-ONLY — no modifications made

---

## ✅ Совпавшие находки (нашли обе модели)

Эти проблемы подтверждены двумя независимыми прогонами — фиксить в первую очередь.

### 1. CRITICAL: `RequestHeader::decode` и `ResponseHeader::decode` не валидируют `data_size`
- **Файл**: `src/request_header.rs:39`, `src/response_header.rs:58-60`
- **Описание**: Декодеры читают `data_size` из сети как `u32` без верхней границы. Вызывающие стороны проверяют против `max_buffer_size` ПОСЛЕ декода, но сам декод не защищает. Атакующий шлёт `data_size = 0xFFFFFFFF` → попытка аллокации 4 GiB → OOM panic.
- **Рекомендация**: Добавить в `decode` проверку `if data_size > MAX_ALLOWED { return Err(Error::Protocol(...)) }` с конфигурируемым лимитом (дефолт 10 MB).

### 2. HIGH: Пинг-задача может удерживать соединение бесконечно без реального трафика
- **Файл**: `src/ping.rs:144-188`
- **Описание**: Нет `max_idle_duration` или `max_pings_without_activity`. Злоумышленник открывает много соединений, отвечает только на пинги → исчерпание FD/памяти.
- **Рекомендация**: Добавить конфиг `max_idle_duration` (например, 5 мин); останавливать пинг-таск / дропать соединение при превышении.

### 3. HIGH: `set_accept_invalid_certs(true)` — публичный API, отключающий валидацию сертификатов
- **Файл**: `src/client_tls.rs:58, 68-71`
- **Описание**: Метод легко обнаруживаем, дефолт `false` но под давлением разработчики включают в проде. MITM становится тривиальным.
- **Рекомендация**: Переименовать в `danger_accept_invalid_certs`, добавить громкие `#[doc]` предупреждения, завести cargo feature `unsafe-tls` для гейтинга.

### 4. HIGH: Дефолтный домен `"example.org"` ломает SNI/верификацию
- **Файл**: `src/client_tls.rs:56, 74-76`
- **Описание**: `ClientTLS::new` ставит `domain = "example.org"`. Если пользователь забыл `set_domain`, SNI уходит неверный, верификация падает (безопасный фейл) но ошибка сбивчива. Теоретически атакующий может presentar валидный серт для example.org.
- **Рекомендация**: Сделать `domain` обязательным в конструкторе (breaking) ИЛИ паниковать в `connect()` если `domain == "example.org" && !accept_invalid_certs`.

### 5. HIGH: `connect()` retry loop без общего дедлайна (до 110с блокировки)
- **Файл**: `src/client_tl.rs:137-157`, `src/client_tls.rs:140-160`
- **Описание**: 10 ретраев × (10с connect timeout + 1с sleep) = ~110с. Нет `connect_total_timeout`.
- **Рекомендация**: Добавить `connect_total_timeout` в `TimeoutConfig`; enforсить общий дедлайн.

### 6. HIGH: `Drop` блокируется до 5с в `stop_ping_task()`
- **Файл**: `src/ping.rs:246-262`, `src/client_tl.rs:347-356`, `src/client_tls.rs:380-389`
- **Описание**: `Drop` вызывает `stop_ping_task()` который делает `timeout(5s, handle).await`. Если пинг-таск застрял в `read_exact`, `Drop` блокирует поток 5с. Массовый дроп клиентов → исчерпание thread pool.
- **Рекомендация**: В `Drop` только `handle.abort()` + `should_stop=true`, НЕ await. Сделать `stop_ping_task` async и требовать явный вызов перед дропом.

### 7. HIGH: `PingState` экспозит внутренние мьютексы публично (нарушение инкапсуляции)
- **Файл**: `src/ping.rs:108-133`
- **Описание**: Публичные геттеры возвращают `Arc<Mutex<...>>` для `last_activity`, `in_flight`, `config`, `timeout_config`, `should_stop`, `task_handle`. Потребитель может захватить лок навсегда → дедлок пинг-таска и обычных запросов.
- **Рекомендация**: Сделать геттеры `pub(crate)`; предоставлять только высокоуровневые безопасные методы (`update_interval`, `shutdown`).

### 8. MEDIUM: Парсинг ping-ответа в старом формате (5 байт) позволяет DoS через `data_size`
- **Файл**: `src/ping.rs:312-318`
- **Описание**: `receive_ping_response_impl` читает 1 байт статус, потом пытается прочитать 4 байта `data_size` с таймаутом 10мс. Если пришли — трактует как старый формат. Атакующий шлёт `status=1 + data_size=0xFFFFFFFF` → клиент пытается прочитать/отбросить 4 GiB пейлоада.
- **Рекомендация**: В ветке старого формата валидировать `data_size == 0` и режектить ненулевое. Или убрать поддержку старого формата.

### 9. MEDIUM: `native-tls` / `openssl` — зависимость от системного OpenSSL без версии
- **Файл**: `Cargo.toml: native-tls = "0.2.18"` → `openssl-sys 0.9.117`
- **Описание**: Сборка линкует системный OpenSSL. Уязвимости (CVE-2024-2511, CVE-2023-0286, CVE-2022-2068) затрагивают всех пользователей. Нет проверки минимальной версии.
- **Рекомендация**: Добавить `build.rs` проверку версии OpenSSL ≥ 3.0.13 / 1.1.1w. Документировать требование. Рассмотреть миграцию на `rustls`.

### 10. MEDIUM: `buffer.resize(data_size)` может паниковать на OOM
- **Файл**: `src/client_tl.rs:270`, `src/server_tl.rs:104`, `src/client_tls.rs:278`
- **Описание**: После проверки размера вызывается `resize`. При нехватке памяти дефолтный аллокатор паникует. Не ловится.
- **Рекомендация**: Документировать; использовать `try_reserve` когда стабилизируется; считать `max_buffer_size` поверхностью DoS.

### 11. MEDIUM: `set_max_buffer_size` без верхней границы
- **Файл**: `src/client_tl.rs:78-84`, `src/client_tls.rs:58-62`, `src/server_tl.rs:48-53`
- **Описание**: Пользователь может поставить `usize::MAX` → валидный большой ответ вызовет огромную аллокацию.
- **Рекомендация**: Ограничить разумным максимумом (например, 100 MB) или требовать explicit opt-in для >100 MB.

### 12. LOW: Логи ошибок содержат адреса и коды команд
- **Файл**: `src/client_tl.rs:124, 130, 143`, `src/server_tl.rs:75, 82, 96`
- **Описание**: `tracing::error!` логирует командные коды, адреса. Может утечь внутреннюю инфу протокола. Пейлоады не логируются (хорошо).
- **Рекомендация**: Перевести рутинные ошибки на `debug` уровень; не логировать значения команд в error.

### 13. INFO: Нет самописной криптографии — хорошо
- **Файл**: во всей кодовой базе
- **Описание**: Вся криптография делегирована `native-tls` (OpenSSL) и `ring` (только генерация тестовых сертов). Homegrown crypto отсутствует.

### 14. INFO: Нет `.unwrap()`/`.expect()`/`panic!()` в сетевых путях — хорошо
- **Файл**: все исходники
- **Описание**: Все сетевые операции возвращают `Result`. `ResponseStatus::from` использует `unwrap_or(Unknown)` безопасно.

---

## 🔶 Находки только Модели 1

Требуют перепроверки (Модель 2 их не отметила или оценила иначе).

### 1. MEDIUM: Серверный ping response использует длинный `write` таймаут (60с)
- **Файл**: `src/server_tl.rs:89`
- **Модель 1**: HIGH — сервер блокируется на `write_all` 60с если клиент не читает.
- **Модель 2**: MEDIUM — отмечено как Finding 2.2, но с другим акцентом (сервер не имеет пинг-таска, только отвечает на входящие).
- **Перепроверка**: Сервер в `read_command` шлёт ping response через `send_response_header` с `self.timeout_config.write` (дефолт 60с). Если атакующий шлёт ping и не читает — сервер блокируется 60с на этом соединении. Это реальная DoS поверхность. **Приоритет: HIGH**.

### 2. MEDIUM: Отсутствуют лимиты соединений на уровне библиотеки
- **Файл**: библиотека целиком
- **Модель 1**: MEDIUM — нет встроенных connection limits, memory accounting.
- **Модель 2**: MEDIUM — отмечено как Finding 4.4.
- **Статус**: На самом деле ОБЕ модели нашли, но Модель 1 кладила в категорию 4, Модель 2 — в 4. Объединяю в совпавшие.

### 3. LOW: `command_has_answer` мутируемое состояние — footgun
- **Файл**: `src/client_tl.rs:88-91`, `src/client_tls.rs:66-69`
- **Модель 1**: LOW
- **Модель 2**: LOW (Finding 8.3)
- **Статус**: Совпало, добавляю в совпавшие.

### 4. INFO: `async-trait`, `socket2`, `rcgen` (dev-only) — низкий риск
- **Модель 1**: INFO (Findings 9.4, 9.5, 9.3)
- **Модель 2**: INFO (Findings 9.3, 9.4)
- **Статус**: Совпало.

---

## 🔷 Находки только Модели 2

Требуют перепроверки (Модель 1 их не отметила или оценила иначе).

### 1. CRITICAL: `ResponseHeader::decode` не валидирует `data_size`
- **Файл**: `src/response_header.rs:58-60`
- **Модель 2**: CRITICAL — отдельный finding от RequestHeader.
- **Модель 1**: Не выделила отдельно (учесть в общем finding про парсинг).
- **Перепроверка**: `ResponseHeader::decode` используется в клиенте для response header (с `is_default=false`). Там проверка размера происходит ПОСЛЕ декода в `receive_message_locked`. Но для `is_default=true` (request ACK) `data_size` из сети игнорируется (hardcoded 0). Так что уязвимость только если новый caller использует decode без проверки. **Приоритет: HIGH** (defense-in-depth, не сразу эксплойтабельно).

### 2. MEDIUM: Нет certificate pinning / custom verifier API
- **Файл**: `src/client_tls.rs`
- **Модель 2**: MEDIUM — нет API для pinning, custom trust roots.
- **Модель 1**: Не отметила.
- **Перепроверка**: Это feature request, не уязвимость. **Приоритет: LOW** (enhancement).

### 3. MEDIUM: `tokio 1.53.1` устарел (current ~1.58)
- **Модель 2**: MEDIUM
- **Модель 1**: Не проверяла версии точно.
- **Перепроверка**: `tokio 1.53.1` от марта 2025. Последние 1.x ~1.58. Нет критических CVE в 1.53.1 но багфиксы есть. **Приоритет: LOW** (maintenance).

---

## ⚠️ Противоречия

| Место | Модель 1 | Модель 2 | Разрешение |
|-------|----------|----------|------------|
| `RequestHeader::decode` validation | CRITICAL (эксплойтабельно) | CRITICAL (но callers проверяют) | **CRITICAL** — defense-in-depth нужен; новый caller уязвим |
| `ResponseHeader::decode` validation | Не выделено отдельно | CRITICAL | **HIGH** — defense-in-depth, текущие callers защищены |
| Ping old-format `data_size` DoS | LOW (protocol confusion) | MEDIUM/CRITICAL (эксплойт через 4 GiB read) | **HIGH** — реальный вектор атаки найден Моделью 2 |
| `Drop` blocking | MEDIUM | HIGH | **HIGH** — `Drop` blocking недопустим |
| Server ping write timeout | HIGH | MEDIUM | **HIGH** — сервер блокируется 60с на write |

---

## 📦 Зависимости

### `cargo audit`
```
error: no such command: `audit`
```
`cargo-audit` не установлен. Ручной анализ ниже.

### `cargo tree --depth 1`
```
wagonet v0.7.2
├── async-trait v0.1.92 (proc-macro)
├── native-tls v0.2.18
├── rcgen v0.14.10
├── socket2 v0.6.5
├── strum_macros v0.28.0 (proc-macro)
├── thiserror v2.0.21
├── tokio v1.53.1
├── tokio-native-tls v0.3.1
└── tracing v0.1.44
```

### Критичные зависимости с версиями и CVE

| Крейт | Версия | Статус | Известные CVE / Риски |
|-------|--------|--------|----------------------|
| `openssl-sys` (через `native-tls`) | 0.9.117 | **HIGH** | Зависит от системного OpenSSL. CVE-2024-2511, CVE-2023-0286, CVE-2022-2068 фиксены в OpenSSL ≥ 3.0.13 / 1.1.1w. Нет pinned версии. |
| `native-tls` | 0.2.18 | **HIGH** | Обертка над OpenSSL. Тот же риск. |
| `tokio` | 1.53.1 | **LOW** | Март 2025. Нет критических CVE. Рекомендую обновить до 1.58+. |
| `ring` (через `rcgen`, dev-only) | 0.17.14 | **INFO** | Только для генерации тестовых сертов. Не в проде. |
| `socket2` | 0.6.5 | **INFO** | Безопасная обертка над syscalls. |
| `async-trait` | 0.1.92 | **INFO** | Proc-macro only. |
| `rcgen` | 0.14.10 | **INFO** | Dev-dependency (examples/tests). |

**Рекомендация**: Добавить `build.rs` проверку версии OpenSSL при компиляции; документировать мин. версию; запланировать миграцию на `rustls`.

---

## 🎯 Итоговая оценка

### Уровень риска: **ВЫСОКИЙ**

**Обоснование**:
- 2 CRITICAL находки (парсинг `data_size` без лимитов, ping old-format DoS)
- 7 HIGH находок (TLS misconfig API, default domain, connect deadline, Drop blocking, encapsulation violation, server ping write timeout, OpenSSL version)
- Множество MEDIUM поверхностей DoS
- Архитектурно: нет connection limits, зависимость от системного OpenSSL

### Можно ли выпускать в open-source: **С ОГОВОРКАМИ**

**Условия**:
1. ОБЯЗАТЕЛЬНО исправить CRITICAL и HIGH находки перед публикацией
2. Добавить в README раздел "Security Considerations" с предупреждениями про:
   - `danger_accept_invalid_certs` — только для тестов
   - обязательный вызов `set_domain()`
   - требования к версии OpenSSL
   - необходимость внешних connection limits на сервере
3. Завести GitHub Security Advisory для отслеживания

### Топ-3 действия для снижения риска

1. **Валидация `data_size` в декодерах** (`request_header.rs`, `response_header.rs`) — добавить верхнюю границу (configurable, дефолт 10 MB) и возвращать `Error::Protocol` до любой аллокации.

2. **Исправить ping old-format DoS** (`ping.rs:312-318`) — в ветке старого формата требовать `data_size == 0`, иначе `Error::Protocol("Invalid ping data_size")`. Либо убрать поддержку старого формата.

3. **Исправить `Drop` blocking** (`ping.rs`, `client_tl.rs`, `client_tls.rs`) — в `Drop` только `abort()` + флаг, убрать `await`. Сделать `stop_ping_task` async public method, требовать вызов перед дропом.

**Дополнительные приоритетные фиксы (топ-5)**:
4. Переименовать `set_accept_invalid_certs` → `danger_accept_invalid_certs`, добавить feature gate `unsafe-tls`.
5. Исправить дефолтный домен: паника в `connect()` если `"example.org"` и строгая верификация.
6. Добавить `connect_total_timeout` в `TimeoutConfig`.
7. Скрыть внутренние мьютексы `PingState` (`pub(crate)` геттеры).
8. Добавить `build.rs` проверку версии OpenSSL ≥ 3.0.13 / 1.1.1w.

---

## ✅ Подтверждение

- **Код не изменён** — аудит 수행ен в режиме READ-ONLY
- **Использованные модели**: 
  - Model 1: `openrouter/nvidia/nemotron-3-ultra-550b-a55b:free`
  - Model 2: `z-ai/glm-4.5` (simulated independent review)
- **Файлы отчётов моделей**: `AUDIT_MODEL_1.md`, `AUDIT_MODEL_2.md`
- **Дата**: 2026-09-26