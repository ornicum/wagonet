# Roadmap to Open Source Publication (v1.0.0)

## Current Status: ~7/10 ready

**Текущие сильные стороны:**
- ✅ Рабочий кастомный бинарный протокол (1 байт для empty payload, 5 байт для data)
- ✅ Поддержка TCP и TLS (native-tls) с безопасными дефолтами (`accept_invalid_certs=false`)
- ✅ 33+ passing tests, включая тесты ping-keep-alive и edge cases
- ✅ Проведены и задокументированы **два независимых security-аудита** (критические фиксы OOM/DoS/deadlock применены)
- ✅ Устранены магические числа (все таймауты и лимиты вынесены в именованные константы)
- ✅ Устранено дублирование: лимит размера данных теперь единый (`max_buffer_size`), принцип наименьшего удивления соблюдён
- ✅ Асинхронный ping-keep-alive без блокировки `Drop`
- ✅ Транспорт агностичен к формату payload (protobuf, bincode, custom binary)

---

## 🚨 PHASE 1: CRITICAL (Must Have Before v1.0.0)

### 1. High-Level API Architecture (Главная цель)
Переход от "сырого транспорта" к фреймворку, где пользователь описывает только бизнес-логику.

**Архитектура `ProtocolCommandHandler`:**
```rust
/// Результат обработки команды хендлером
pub enum CommandResult {
    /// Успешный ответ с данными (status=0, payload)
    Ok(Vec<u8>),
    /// Команда без ответа (fire-and-forget, status игнорируется)
    NoAnswer,
    /// Ошибка с кодом статуса и опциональным сообщением
    Error { status: u8, data: Option<Vec<u8>> },
}

/// Пользователь реализует этот трейт для обработки входящих команд.
/// Формат payload (protobuf, bincode, raw bytes) определяется пользователем.
#[async_trait::async_trait]
pub trait ProtocolCommandHandler: Send + Sync + 'static {
    async fn handle(&self, command: u32, data: &[u8]) -> CommandResult;

    // Опциональные хуки
    async fn on_connected(&self, _addr: std::net::SocketAddr) {}
    async fn on_disconnected(&self, _addr: std::net::SocketAddr) {}
}
```

**Архитектура `Server::run()`:**
```rust
pub struct Server<H: ProtocolCommandHandler> {
    address: String,
    handler: std::sync::Arc<H>,
    timeout_config: TimeoutConfig,
}

impl<H: ProtocolCommandHandler> Server<H> {
    pub fn new(address: String, handler: H) -> Self { /* ... */ }
    pub fn set_timeout_config(&mut self, config: TimeoutConfig) { /* ... */ }

    /// Основной цикл: accept -> spawn task -> read_command -> handler.handle -> send_data
    pub async fn run(self) -> crate::Result<()> {
        let listener = tokio::net::TcpListener::bind(&self.address).await?;
        loop {
            let (stream, addr) = listener.accept().await?;
            let handler = self.handler.clone();
            let config = self.timeout_config.clone();

            tokio::spawn(async move {
                // 1. Инициализация ServerTL или ServerTLS
                // 2. Цикл: loop { match server.read_command().await { Ok(Some(cmd, size)) => ... } }
                // 3. Вызов: let result = handler.handle(cmd, &data).await;
                // 4. Отправка: server.send_data(result.status, result.data).await;
            });
        }
    }
}
```

### 2. Documentation & Community
- [ ] **README.md**: Обновить Quick Start, показав *новый* высокоуровневый API.
- [ ] **examples/**: Добавить `high_level_server.rs`, `high_level_client.rs`, `protobuf_example.rs`.
- [ ] **docs/architecture.md**: Обновить диаграммы, добавив слой `ProtocolCommandHandler`.
- [ ] **SECURITY.md**: Инструкция по приватному репорту уязвимостей.
- [ ] **CONTRIBUTING.md** и **CODE_OF_CONDUCT.md**.

### 3. CI/CD Pipeline (`.github/workflows/ci.yml`)
- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo test --all`
- [ ] `cargo audit` (проверка зависимостей)

### 4. Cargo.toml Metadata
- [ ] `description`, `repository`, `license = "MIT OR Apache-2.0"`
- [ ] `keywords = ["tcp", "tls", "async", "transport", "binary-protocol"]`
- [ ] `categories = ["network-programming", "asynchronous"]`

---

## ⚠️ PHASE 2: IMPORTANT (Quality & Reliability)

### 1. Testing & Fuzzing
- [ ] **Integration tests**: Клиент-сервер roundtrip через новый `Server::run()` и `Client::connect()`.
- [ ] **Edge cases**: Обрыв соединения посередине сообщения, конкатенация фреймов.
- [ ] **Fuzzing**: Добавить `cargo-fuzz` таргет для `RequestHeader::decode` и `ResponseHeader::decode` (защита от регрессий OOM).

### 2. Error Handling & Logging
- [ ] Убедиться, что все `Box<dyn Error>` заменены на типизированный `wagonet::Error` (через `thiserror`).
- [ ] Заменить остаточные `println!` на `tracing::debug!` / `tracing::warn!`.
- [ ] Добавить `tracing::Span` на время обработки одной команды в `Server::run()`.

### 3. API Polish
- [ ] Добавить `wagonet::prelude::*` для удобного импорта.
- [ ] Реализовать `Client` с аналогичным высокоуровневым API (методы `call(command, data)` и `call_no_answer(command, data)`).

### 4. Пример использования с protobuf (Prost)
Добавить в документацию и examples/, показывающий что wagonet не навязывает формат сериализации:

```rust
use prost::Message;
use wagonet::{ProtocolCommandHandler, CommandResult};

#[derive(Clone, PartialEq, Message)]
pub struct GreetRequest {
    #[prost(string, tag = "1")]
    pub name: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct GreetResponse {
    #[prost(string, tag = "1")]
    pub greeting: String,
}

struct MyHandler;

#[async_trait::async_trait]
impl ProtocolCommandHandler for MyHandler {
    async fn handle(&self, command: u32, data: &[u8]) -> CommandResult {
        match command {
            1 => {
                let req = match GreetRequest::decode(data) {
                    Ok(r) => r,
                    Err(_) => return CommandResult::Error { status: 1, data: None },
                };
                let resp = GreetResponse { greeting: format!("Hello, {}!", req.name) };
                let mut buf = Vec::new();
                resp.encode(&mut buf).unwrap();
                CommandResult::Ok(buf)
            }
            _ => CommandResult::Error { status: 255, data: None },
        }
    }
}
```

---

## 🌟 PHASE 3: NICE TO HAVE (Production-Ready)

### 1. Performance
- [ ] Добавить `benches/` с использованием `criterion` (throughput сообщений/сек, latency percentiles).
- [ ] Оценить возможность buffer pooling (например, через `bytes::BytesMut`) для high-throughput сценариев.

### 2. Extensibility
- [ ] Feature flags в `Cargo.toml`: `default = ["tls"]`, `tls = ["native-tls", "tokio-native-tls"]`.
- [ ] Поддержка сжатия (опционально, через feature flag `compression = ["flate2"]`).

### 3. Observability
- [ ] Хуки для метрик (количество обработанных команд, ошибки, время жизни соединения).

---

## 📅 SUGGESTED TIMELINE

### Неделя 1: "Архитектурный сдвиг"
- Завершить рефакторинг `max_data_size` -> `max_buffer_size`.
- Спроектировать и реализовать `ProtocolCommandHandler` и `Server::run()`.
- Написать примеры `high_level_server.rs` и `protobuf_example.rs`.

### Неделя 2: "Надёжность и Инфраструктура"
- Настроить GitHub Actions CI (fmt, clippy, test, audit).
- Написать интеграционные тесты для нового API.
- Добавить `SECURITY.md`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`.

### Неделя 3: "Полировка и Релиз"
- Обновить README и документацию (`cargo doc`).
- Добавить бенчмарки (`criterion`).
- Прогнать финальный fuzzing.
- **Релиз v1.0.0** на crates.io.

---

## 🎯 PRIORITY ORDER

1. **High-Level API** (`ProtocolCommandHandler` + `Server::run()`) — *это главная ценность проекта*.
2. **CI/CD Pipeline** — *гарантия того, что рефакторинг ничего не сломал*.
3. **Документация и Примеры** — *пользователь должен понять, как это использовать, за 2 минуты*.
4. **Fuzzing и Edge-case тесты** — *подтверждение заявленной безопасности*.
5. **Бенчмарки и Feature Flags** — *конкурентное преимущество*.

---

## 📝 NOTES

- **Semantic Versioning**: Текущая версия `0.x`. Любые изменения в сигнатуре `ProtocolCommandHandler` до `1.0.0` допустимы, но после `1.0.0` потребуют мажорного релиза.
- **Ниша проекта**: wagonet — это транспортный слой, заменяющий HTTP/2 из gRPC. Он работает с любым форматом payload (protobuf через Prost, bincode, custom binary), давая полный контроль над транспортом без overhead HTTP/2. Пользователь сам решает, как сериализовать данные, а wagonet обеспечивает надёжную доставку с настраиваемыми таймаутами, keep-alive и безопасностью.
- **Не замена protobuf**: wagonet не конкурирует с protobuf/gRPC на уровне сериализации. Он заменяет транспорт (HTTP/2 -> TCP/TLS), а формат payload остаётся на усмотрение пользователя.
- **Security First**: Наличие файлов `AUDIT_*.md` в репозитории — это мощное маркетинговое преимущество. Не удалять их.

---

## 🔗 RESOURCES

- [Rust API Guidelines](https://rust-lang.github.io/api-guidelines/)
- [crates.io publishing guide](https://doc.rust-lang.org/cargo/reference/publishing.html)
- [Semantic Versioning](https://semver.org/)
- [Prost (protobuf for Rust)](https://github.com/tokio-rs/prost)
- [cargo-fuzz book](https://rust-fuzz.github.io/book/)
