pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl Credentials {
    pub fn new(username: String, password: String) -> Self {
        Credentials { username, password }
    }
}