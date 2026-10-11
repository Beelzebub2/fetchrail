use super::*;
use std::{
    io::{BufRead, BufReader},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
};

struct PrivateBus(Child);
impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Policy(Arc<Mutex<String>>);
#[zbus::interface(name = "org.freedesktop.login1.Manager")]
impl Policy {
    fn can_power_off(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

struct Systemd(Arc<Mutex<String>>);
#[zbus::interface(name = "org.freedesktop.systemd1.Manager")]
impl Systemd {
    #[zbus(property)]
    fn version(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

struct Network(Arc<Mutex<String>>);
#[zbus::interface(name = "org.freedesktop.NetworkManager")]
impl Network {
    fn get_permissions(&self) -> HashMap<String, String> {
        HashMap::from([(
            "org.freedesktop.NetworkManager.network-control".into(),
            self.0.lock().unwrap().clone(),
        )])
    }
    #[zbus(property)]
    fn active_connections(&self) -> Vec<OwnedObjectPath> {
        vec![
            OwnedObjectPath::try_from("/org/freedesktop/NetworkManager/ActiveConnection/1")
                .unwrap(),
        ]
    }
}

struct ActiveConnection;
#[zbus::interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
impl ActiveConnection {
    #[zbus(property)]
    fn uuid(&self) -> &str {
        "12345678-1234-1234-1234-123456789abc"
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        "Selected fixture connection"
    }
}

#[tokio::test]
async fn dbus_probes_follow_policy_version_and_connection_identity() {
    // All services below live on a private daemon; the machine's system bus is never used.
    let mut daemon = PrivateBus(
        Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("Linux service checks require dbus-daemon"),
    );
    let mut address = String::new();
    BufReader::new(daemon.0.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let address = address.trim();
    let policy = Arc::new(Mutex::new("challenge".to_string()));
    let version = Arc::new(Mutex::new("256.2".to_string()));
    let network = Arc::new(Mutex::new("auth".to_string()));
    let _server = zbus::connection::Builder::address(address)
        .unwrap()
        .name(LOGIN)
        .unwrap()
        .name("org.freedesktop.systemd1")
        .unwrap()
        .name(NM)
        .unwrap()
        .serve_at(LOGIN_PATH, Policy(policy.clone()))
        .unwrap()
        .serve_at("/org/freedesktop/systemd1", Systemd(version.clone()))
        .unwrap()
        .serve_at(NM_PATH, Network(network.clone()))
        .unwrap()
        .serve_at(
            "/org/freedesktop/NetworkManager/ActiveConnection/1",
            ActiveConnection,
        )
        .unwrap()
        .build()
        .await
        .unwrap();
    let client = zbus::connection::Builder::address(address)
        .unwrap()
        .build()
        .await
        .unwrap();
    let (shutdown, force) = power_features(&client).await.unwrap();
    assert!(shutdown.available);
    assert!(
        !force.available,
        "Older logind must not offer inhibitor bypass"
    );
    *version.lock().unwrap() = "257.2".into();
    assert!(power_features(&client).await.unwrap().1.available);
    let (disconnect, connections) = network_connections(&client).await.unwrap();
    assert!(disconnect.available);
    assert_eq!(connections.len(), 1);
    assert_eq!(connections[0].1.id, "12345678-1234-1234-1234-123456789abc");
    assert_eq!(connections[0].1.name, "Selected fixture connection");
    *policy.lock().unwrap() = "no".into();
    *network.lock().unwrap() = "no".into();
    let (shutdown, force) = power_features(&client).await.unwrap();
    assert!(!shutdown.available && !force.available);
    assert!(!network_connections(&client).await.unwrap().0.available);
}
