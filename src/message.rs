#[derive(Clone)]
pub struct UsbDevice {
    pub path: String,
    pub label: String,
    pub size: u64,
}

pub enum Message {
    Progress(u64, u64),
    Done(bool, Option<String>),
    Log(String),
    Status(String),
    VerifyProgress(f32),
    VerifyDone(bool, String),
}
