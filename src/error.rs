//! Error taxonomy.
//!
//! Every failure mode a caller might want to react to differently gets its own
//! variant, so nothing has to be recovered by matching on message text.

use std::io;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    // -- Bluetooth --------------------------------------------------------
    /// The host's Bluetooth stack is unusable: no adapter, radio off, or (on
    /// Linux) BlueZ not running.
    #[error(
        "Bluetooth is unavailable: {0}\n\
         Check that this host has a BLE adapter, that Bluetooth is switched on, \
         and — on Linux — that the bluetooth service is running."
    )]
    BluetoothUnavailable(String),

    #[error(
        "no GR camera found over BLE. Check that the camera is paired with this host \
         and that its Bluetooth setting is on (Enable Condition = 'On anytime' is \
         required to reach a powered-off camera)."
    )]
    CameraNotFound,

    #[error("multiple GR cameras in range: {0}. Pass --address to choose one.")]
    AmbiguousCamera(String),

    #[error("BLE: {0}")]
    Ble(String),

    /// A characteristic the camera should expose was not in its GATT table.
    #[error("the camera does not expose characteristic {0} — is this really a GR III?")]
    MissingCharacteristic(uuid::Uuid),

    /// A characteristic answered with bytes that do not fit the documented
    /// layout. Surfaced rather than guessed at, because guessing here would
    /// mean writing a wrong value back to the camera.
    #[error("characteristic {uuid} returned {got} bytes, expected {want}")]
    BadCharacteristicValue {
        uuid: uuid::Uuid,
        got: usize,
        want: &'static str,
    },

    // -- HTTP -------------------------------------------------------------
    #[error("{0}")]
    Http(String),

    /// The camera answered, but with a non-200 `errCode` in its JSON envelope.
    #[error("{endpoint}: errCode={code} errMsg={message:?}")]
    CameraApi {
        code: i64,
        message: String,
        endpoint: String,
    },

    #[error(
        "camera did not answer at {host} within {secs}s. \
         Is this host associated with the camera's Wi-Fi network?"
    )]
    CameraUnreachable { host: String, secs: u64 },

    // -- host Wi-Fi -------------------------------------------------------
    #[error("{0}")]
    Network(String),

    #[error("no usable Wi-Fi backend on this host")]
    NoWifiBackend,

    // -- everything else --------------------------------------------------
    #[error("battery at {level}% is below the {floor}% floor; charge the camera or pass --min-battery 0")]
    BatteryTooLow { level: i8, floor: i8 },

    #[error("{0}")]
    Config(String),

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
}

impl Error {
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }

    /// Process exit code. Distinguishing "the sync ran but some files failed"
    /// (1) from "it could not run at all" (2) lets a wrapper retry only the
    /// second case.
    pub fn exit_code(&self) -> i32 {
        2
    }
}
