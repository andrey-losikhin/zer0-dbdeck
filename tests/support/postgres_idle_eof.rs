use super::*;
use sqlx::postgres::{PgConnectOptions, PgListener, PgPoolOptions, PgSslMode};
use std::sync::atomic::AtomicUsize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

const POSTGRES_PORT: u16 = 55432;
const TEST_TIMEOUT: Duration = Duration::from_secs(15);

fn options(port: u16) -> PgConnectOptions {
    PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .database("dbdeck_test")
        .username("dbdeck_test")
        .password(&std::env::var("DBDECK_PG_PASSWORD").expect("set local test password"))
        .ssl_mode(PgSslMode::Disable)
}

enum Interrupt {
    Cut,
    HoldReplies {
        ready: oneshot::Sender<()>,
        cut: oneshot::Receiver<()>,
    },
}

pub(crate) struct Proxy {
    pub(crate) port: u16,
    interrupt: Option<oneshot::Sender<Interrupt>>,
    cut_done: Option<oneshot::Receiver<()>>,
    connections: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Proxy {
    pub(crate) async fn start(reject_replacement: bool) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (interrupt, command) = oneshot::channel();
        let (closed, cut_done) = oneshot::channel();
        let connections = Arc::new(AtomicUsize::new(0));
        let accepted = Arc::clone(&connections);
        let task = tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            accepted.fetch_add(1, Ordering::Relaxed);
            let mut server = TcpStream::connect(("127.0.0.1", POSTGRES_PORT))
                .await
                .unwrap();
            let forward = Box::pin(tokio::io::copy_bidirectional(&mut client, &mut server));
            use futures_util::future::{Either, select};
            let command = match select(forward, command).await {
                Either::Left((result, _)) => panic!("connection ended before cut: {result:?}"),
                Either::Right((command, _)) => command.unwrap(),
            };
            if let Interrupt::HoldReplies { ready, cut } = command {
                ready.send(()).unwrap();
                let forward = Box::pin(tokio::io::copy(&mut client, &mut server));
                match select(forward, cut).await {
                    Either::Left((result, _)) => panic!("request forwarding ended: {result:?}"),
                    Either::Right((result, _)) => result.unwrap(),
                }
            }
            client.shutdown().await.unwrap();
            closed.send(()).unwrap();
            drop(client);
            drop(server);

            loop {
                let (mut client, _) = listener.accept().await.unwrap();
                accepted.fetch_add(1, Ordering::Relaxed);
                if reject_replacement {
                    // Read the whole startup packet before FIN to reproduce handshake EOF.
                    let length = client.read_u32().await.unwrap() as usize;
                    assert!((8..=1024).contains(&length));
                    let mut startup = vec![0; length - 4];
                    client.read_exact(&mut startup).await.unwrap();
                    client.shutdown().await.unwrap();
                } else {
                    let mut server = TcpStream::connect(("127.0.0.1", POSTGRES_PORT))
                        .await
                        .unwrap();
                    tokio::io::copy_bidirectional(&mut client, &mut server)
                        .await
                        .unwrap();
                }
            }
        });
        Self {
            port,
            interrupt: Some(interrupt),
            cut_done: Some(cut_done),
            connections,
            task,
        }
    }

    fn interrupt(&mut self, command: Interrupt) {
        assert!(self.interrupt.take().unwrap().send(command).is_ok());
    }

    async fn wait_for_cut(&mut self) {
        self.cut_done.take().unwrap().await.unwrap();
    }

    pub(crate) async fn cut(&mut self) {
        self.interrupt(Interrupt::Cut);
        self.wait_for_cut().await;
    }

    fn accepted(&self) -> usize {
        assert!(!self.task.is_finished(), "proxy stopped unexpectedly");
        self.connections.load(Ordering::Relaxed)
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn run(test: impl std::future::Future<Output = ()>) {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async { tokio::time::timeout(TEST_TIMEOUT, test).await.unwrap() });
}

pub(crate) fn session(port: u16) -> SqlSession {
    let mut session = SqlSession::new(DbPool::PostgreSql(
        PgPoolOptions::new().connect_lazy_with(options(port)),
    ));
    session.schema = Some("pg_catalog".to_owned());
    session
}

async fn run_sql(session: &SqlSession, sql: &str) -> (Result<QueryResult, DbError>, bool) {
    session.execute_with_recovery(sql, 10, false).await
}

pub(crate) async fn mark_idle(session: &SqlSession) {
    session.inner.lock().await.last_activity = Some(Instant::now() - POSTGRES_IDLE_AFTER);
}

async fn notifications(channel: &str) -> PgListener {
    let pool = PgPoolOptions::new()
        .connect_with(options(POSTGRES_PORT))
        .await
        .unwrap();
    let mut listener = PgListener::connect_with(&pool).await.unwrap();
    listener.listen(channel).await.unwrap();
    listener
}

async fn assert_no_notification(listener: &mut PgListener) {
    assert!(
        tokio::time::timeout(Duration::from_millis(200), listener.recv())
            .await
            .is_err()
    );
}

#[test]
#[ignore = "requires local PostgreSQL at 127.0.0.1:55432"]
fn postgres_sql_tab_idle_eof_recovers_once() {
    run(async {
        let mut proxy = Proxy::start(false).await;
        let mut listener = notifications("dbdeck_sql_eof_recovered").await;
        let session = session(proxy.port);
        let (result, reconnected) = run_sql(&session, "SELECT 1").await;
        result.unwrap();
        assert!(!reconnected);
        let previous_backend = session.backend_id.load(Ordering::Acquire);
        assert_ne!(previous_backend, 0);
        mark_idle(&session).await;
        proxy.cut().await;

        let sql = "SELECT pg_notify('dbdeck_sql_eof_recovered', 'recovered'), current_schema(), current_setting('lock_timeout')";
        let (result, reconnected) = run_sql(&session, sql).await;
        assert!(reconnected);
        let result = result.unwrap();
        assert_eq!(
            result.data.rows[0][1],
            crate::database::CellValue::Text("pg_catalog".into())
        );
        assert_eq!(
            result.data.rows[0][2],
            crate::database::CellValue::Text("5s".into())
        );
        assert_ne!(session.backend_id.load(Ordering::Acquire), previous_backend);
        assert_eq!(listener.recv().await.unwrap().payload(), "recovered");
        assert_no_notification(&mut listener).await;
        let (result, reconnected) = run_sql(&session, "SELECT 2").await;
        result.unwrap();
        assert!(!reconnected);
        assert_eq!(proxy.accepted(), 2);
        session.close().await;
    });
}

#[test]
#[ignore = "requires local PostgreSQL at 127.0.0.1:55432"]
fn postgres_sql_tab_idle_eof_failed_replacement_is_bounded() {
    run(async {
        let mut proxy = Proxy::start(true).await;
        let session = session(proxy.port);
        session.execute("SELECT 1").await.unwrap();
        mark_idle(&session).await;
        proxy.interrupt(Interrupt::Cut);
        proxy.wait_for_cut().await;

        let (result, reconnected) = run_sql(&session, "SELECT 2").await;
        let error = result.unwrap_err();
        assert_eq!(error.kind, DbErrorKind::Disconnected);
        assert!(
            error
                .message
                .contains("expected to read 5 bytes, got 0 bytes at EOF")
        );
        assert!(error.message.contains("SQL не отправлен"));
        assert!(!reconnected);
        assert!(session.is_closed());
        assert!(run_sql(&session, "SELECT 3").await.0.is_err());
        assert_eq!(proxy.accepted(), 2);
    });
}

#[test]
#[ignore = "requires local PostgreSQL at 127.0.0.1:55432"]
fn postgres_sql_tab_idle_eof_loses_open_transaction() {
    run(async {
        for begin in [
            Some("BEGIN"),
            Some(
                "BEGIN; CREATE FUNCTION pg_temp.dbdeck_eof_fn() RETURNS int LANGUAGE SQL BEGIN ATOMIC SELECT 1; END;",
            ),
            Some(r"BEGIN; SELECT '\'; COMMIT; --';"),
            None,
        ] {
            let mut proxy = Proxy::start(false).await;
            let session = session(proxy.port);
            if let Some(begin) = begin {
                if begin.contains("\\'") {
                    session
                        .execute("SET standard_conforming_strings = off")
                        .await
                        .unwrap();
                }
                session.execute(begin).await.unwrap();
            } else {
                session
                    .transaction(TransactionAction::Manual)
                    .await
                    .unwrap();
            }
            session.execute("SELECT 1").await.unwrap();
            assert!(session.has_pending_transaction());
            mark_idle(&session).await;
            proxy.interrupt(Interrupt::Cut);
            proxy.wait_for_cut().await;

            let (result, reconnected) = run_sql(&session, "SELECT 2").await;
            let error = result.unwrap_err();
            assert_eq!(error.kind, DbErrorKind::Disconnected);
            assert!(error.message.contains("Открытая транзакция потеряна"));
            assert!(error.message.contains("SQL не отправлен"));
            assert!(!reconnected);
            assert!(session.is_closed());
            assert_eq!(proxy.accepted(), 1);
        }
    });
}

#[test]
#[ignore = "requires local PostgreSQL at 127.0.0.1:55432"]
fn postgres_sql_tab_idle_eof_keeps_live_aborted_transaction() {
    run(async {
        let session = session(POSTGRES_PORT);
        session
            .transaction(TransactionAction::Manual)
            .await
            .unwrap();
        assert!(session.execute("SELECT 1 / 0").await.is_err());
        mark_idle(&session).await;

        let (result, reconnected) = run_sql(&session, "SELECT 2").await;
        assert_eq!(result.unwrap_err().kind, DbErrorKind::Other);
        assert!(!reconnected);
        assert!(session.has_pending_transaction());
        assert!(!session.is_closed());
        session
            .transaction(TransactionAction::Rollback)
            .await
            .unwrap();
        session.execute("SELECT 3").await.unwrap();
        session.close().await;
    });
}

#[test]
#[ignore = "requires local PostgreSQL at 127.0.0.1:55432"]
fn postgres_sql_tab_idle_eof_after_dispatch_does_not_replay() {
    run(async {
        let mut proxy = Proxy::start(false).await;
        let mut listener = notifications("dbdeck_sql_eof_dispatched").await;
        let session = session(proxy.port);
        session.execute("SELECT 1").await.unwrap();
        let (ready, holding) = oneshot::channel();
        let (cut, cut_request) = oneshot::channel();
        proxy.interrupt(Interrupt::HoldReplies {
            ready,
            cut: cut_request,
        });
        holding.await.unwrap();

        let worker = session.clone();
        let task = tokio::spawn(async move {
            run_sql(
                &worker,
                "SELECT pg_notify('dbdeck_sql_eof_dispatched', 'dispatched')",
            )
            .await
        });
        assert_eq!(listener.recv().await.unwrap().payload(), "dispatched");
        cut.send(()).unwrap();
        proxy.wait_for_cut().await;
        let (result, reconnected) = task.await.unwrap();
        let error = result.unwrap_err();
        assert_eq!(error.kind, DbErrorKind::Disconnected);
        assert!(error.message.contains("Результат запроса неизвестен"));
        assert!(error.message.contains("Переподключитесь вручную"));
        assert!(!reconnected);
        assert!(session.is_closed());
        assert_no_notification(&mut listener).await;
        assert_eq!(proxy.accepted(), 1);
    });
}
