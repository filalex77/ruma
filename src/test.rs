use std::convert::TryFrom;
use std::fmt::{Debug, Formatter, Result as FmtResult};
use std::sync::Once;

use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel::r2d2::{CustomizeConnection, Error as R2d2DieselError, Pool};
use diesel_migrations::setup_database;
use env_logger;
use iron;
use iron::headers::{ContentType, Headers};
use iron::method::Method;
use iron::status::Status;
use iron_test::{request, response};
use mount::Mount;
use ruma_events::presence::PresenceState;
use ruma_identifiers::UserId;
use serde_json::{from_str, to_string, Value};

use crate::config::Config;
use crate::embedded_migrations::run as run_pending_migrations;
use crate::models::pusher::PusherOptions;
use crate::query::{Batch, SyncOptions};
use crate::server::Server;

static START: Once = Once::new();

const DATABASE_URL: &str = "postgres://postgres:test@postgres:5432/ruma_test";
const POSTGRES_URL: &str = "postgres://postgres:test@postgres:5432";

/// Used to return the randomly generated user id and access token
#[derive(Debug)]
pub struct TestUser {
    pub id: String,
    pub token: String,
    pub name: String,
}

impl TestUser {
    pub fn new(user: UserId, token: String) -> Self {
        Self {
            id: user.to_string(),
            token,
            name: user.localpart().to_string(),
        }
    }
}

/// Manages the Postgres database for the duration of a test case and provides helper methods for
/// interacting with the Ruma API server.
pub struct Test {
    mount: Mount,
}

impl Debug for Test {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.debug_struct("Server")
            .field("mount", &"Mount { ... }")
            .finish()
    }
}

/// An HTTP response from the server.
#[derive(Debug)]
pub struct Response {
    pub body: String,
    pub headers: Headers,
    json: Option<Value>,
    pub status: Status,
}

/// An r2d2 plugin for starting a test transaction whenever a database connection is acquired from
/// the connection pool.
#[derive(Clone, Copy, Debug)]
pub struct TestTransactionConnectionCustomizer;

impl CustomizeConnection<PgConnection, R2d2DieselError> for TestTransactionConnectionCustomizer {
    fn on_acquire(&self, conn: &mut PgConnection) -> Result<(), R2d2DieselError> {
        conn.begin_test_transaction()
            .map_err(R2d2DieselError::QueryError)
    }
}

impl Test {
    /// Creates a new `Test`.
    pub fn new() -> Self {
        // Since we don't have control of the `main` function during tests, we initialize the
        // logger here. It will only actually initialize on the first test that is run. Subsequent
        // calls will return an error, but we don't care, so just ignore the result.
        let _ = env_logger::try_init();

        START.call_once(|| {
            if PgConnection::establish(DATABASE_URL).is_ok() {
                let connection = PgConnection::establish(POSTGRES_URL)
                    .expect("Failed to connect to Postgres to drop the existing ruma_test table.");

                connection
                    .execute("DROP DATABASE IF EXISTS ruma_test")
                    .expect("Failed to drop the existing ruma_test table.");
            }

            let pg_connection =
                PgConnection::establish(POSTGRES_URL).expect("Failed to connect to Postgres.");

            pg_connection
                .execute("CREATE DATABASE ruma_test")
                .expect("Failed to create the ruma_test table.");

            let db_connection = PgConnection::establish(DATABASE_URL)
                .expect("Failed to connect to Postgres database.");

            setup_database(&db_connection).expect("Failed to create migrations table.");
            run_pending_migrations(&db_connection).expect("Failed to run migrations.");
        });

        let config = Config {
            bind_address: "127.0.0.1".to_string(),
            bind_port: "0".to_string(),
            domain: "ruma.test".to_string(),
            macaroon_secret_key: "YymznQHmKdN9B4f7iBalJB1tWEDy9LdaFSQJEtB3R5w=".into(),
            postgres_url: DATABASE_URL.to_string(),
        };

        let r2d2_pool_builder = Pool::builder()
            .max_size(1)
            .connection_customizer(Box::new(TestTransactionConnectionCustomizer));

        let server = match Server::new(&config).mount_all_with_options(r2d2_pool_builder, false) {
            Ok(server) => server,
            Err(error) => panic!("Failed to create Iron server: {}", error),
        };

        Self {
            mount: server.into_mount(),
        }
    }

    /// Makes a GET request to the server.
    pub fn get(&self, path: &str) -> Response {
        self.request(Method::Get, path, "")
    }

    /// Makes a POST request to the server.
    pub fn post(&self, path: &str, body: &str) -> Response {
        self.request(Method::Post, path, body)
    }

    /// Makes a DELETE request to the server.
    pub fn delete(&self, path: &str) -> Response {
        self.request(Method::Delete, path, "")
    }

    /// Makes a PUT request to the server.
    pub fn put(&self, path: &str, body: &str) -> Response {
        self.request(Method::Put, path, body)
    }

    /// Makes a request to the server.
    pub fn request(&self, method: Method, path: &str, body: &str) -> Response {
        let mut headers = Headers::new();

        headers.set(ContentType::json());

        let response = match request::request(
            method,
            &format!("http://ruma.test{}", path)[..],
            body,
            headers,
            &self.mount,
        ) {
            Ok(response) => response,
            Err(error) => error.response,
        };

        Response::from_iron_response(response)
    }

    /// Easy check for EmptyResponse modifier.
    pub fn check_empty_response(&self, response: Response) {
        assert_eq!(response.status, Status::Ok);
        assert_eq!(response.body, "{}".to_string());
    }

    /// Registers a new user account and returns the response of the API call.
    pub fn register_user(&self, body: &str) -> Response {
        self.post("/_matrix/client/r0/register", body)
    }

    /// Registers a new user account with a random user id and returns
    /// the `TestUser`
    pub fn create_user(&self) -> TestUser {
        let response = self.register_user(&r#"{"password": "secret"}"#.to_string());

        let access_token = response
            .json()
            .get("access_token")
            .expect("access_token does not exist in response")
            .as_str()
            .expect("access_token is not a string")
            .to_string();

        let user_id = response
            .json()
            .get("user_id")
            .expect("user_id does not exist in response")
            .as_str()
            .expect("user_id is not a string")
            .to_string();

        TestUser::new(UserId::try_from(user_id.as_ref()).unwrap(), access_token)
    }

    /// Creates a room given the body parameters and returns the room ID as a string.
    pub fn create_room_with_params(&self, access_token: &str, body: &str) -> String {
        self.post(
            &format!(
                "/_matrix/client/r0/createRoom?access_token={}",
                access_token
            ),
            body,
        )
        .json()
        .get("room_id")
        .unwrap()
        .as_str()
        .unwrap()
        .to_string()
    }

    /// Creates a room and returns the room ID as a string.
    pub fn create_room(&self, access_token: &str) -> String {
        self.create_room_with_params(access_token, "{}")
    }

    /// Creates a public room and returns the room ID as a string.
    pub fn create_public_room(&self, access_token: &str) -> String {
        self.create_room_with_params(access_token, r#"{"visibility": "public"}"#)
    }

    /// Creates a private room and returns the room ID as a string.
    pub fn create_private_room(&self, access_token: &str) -> String {
        self.create_room_with_params(access_token, r#"{"visibility": "private"}"#)
    }

    /// Invite a `User` to a `Room`.
    pub fn invite(&self, access_token: &str, room_id: &str, invitee_id: &str) -> Response {
        let body = format!(r#"{{"user_id": "{}"}}"#, invitee_id);
        let path = format!(
            "/_matrix/client/r0/rooms/{}/invite?access_token={}",
            room_id, access_token
        );

        self.post(&path, &body)
    }

    /// Kick a `User` from a `Room`.
    pub fn kick_from_room(
        &self,
        access_token: &str,
        room_id: &str,
        user_id: &str,
        reason: Option<&str>,
    ) -> Response {
        let body = format!(
            r#"{{"user_id": "{}", "reason": "{}"}}"#,
            user_id,
            reason.unwrap_or("")
        );
        let path = format!(
            "/_matrix/client/r0/rooms/{}/kick?access_token={}",
            room_id, access_token
        );

        self.post(&path, &body)
    }

    /// Look up a `RoomId` using an alias.
    pub fn get_room_by_alias(&self, alias: &str) -> Response {
        self.get(&format!("/_matrix/client/r0/directory/room/{}", alias))
    }

    /// Join an existent room.
    pub fn join_room(&self, access_token: &str, room_id: &str) -> Response {
        let join_path = format!(
            "/_matrix/client/r0/rooms/{}/join?access_token={}",
            room_id, access_token
        );

        self.post(&join_path, r"{}")
    }

    /// Leave a room.
    pub fn leave_room(&self, access_token: &str, room_id: &str) -> Response {
        let leave_room_path = format!(
            "/_matrix/client/r0/rooms/{}/leave?access_token={}",
            room_id, access_token
        );

        self.post(&leave_room_path, "{}")
    }

    /// Create tag
    pub fn create_tag(
        &self,
        access_token: &str,
        room_id: &str,
        user_id: &str,
        tag: &str,
        content: &str,
    ) {
        let put_tag_path = format!(
            "/_matrix/client/r0/user/{}/rooms/{}/tags/{}?access_token={}",
            user_id, room_id, tag, access_token
        );

        let response = self.put(&put_tag_path, content);
        assert_eq!(response.status, Status::Ok);
    }

    /// Create a filter
    pub fn create_filter(&self, access_token: &str, user_id: &str, content: &str) -> String {
        let filter_path = format!(
            "/_matrix/client/r0/user/{}/filter?access_token={}",
            user_id, access_token
        );

        let response = self.post(&filter_path, content);
        assert_eq!(response.status, Status::Ok);
        response
            .json()
            .get("filter_id")
            .unwrap()
            .as_str()
            .unwrap()
            .to_string()
    }

    /// Send a message to room.
    pub fn send_message(
        &self,
        access_token: &str,
        room_id: &str,
        message: &str,
        txn_id: u64,
    ) -> Response {
        let create_event_path = format!(
            "/_matrix/client/r0/rooms/{}/send/m.room.message/{}?access_token={}",
            room_id, txn_id, access_token
        );
        let body = format!(r#"{{"body":"{}","msgtype":"m.text"}}"#, message);
        self.put(&create_event_path, &body)
    }

    /// Send a state event to a room.
    pub fn send_state_event(
        &self,
        access_token: &str,
        room_id: &str,
        event_type: &str,
        event_content: &str,
    ) -> Response {
        let state_event_path = format!(
            "/_matrix/client/r0/rooms/{}/state/{}?access_token={}",
            room_id, event_type, access_token
        );

        self.put(&state_event_path, event_content)
    }

    /// Create a User and Room.
    pub fn initial_fixtures(&self, body: &str) -> (TestUser, String) {
        let user = self.create_user();
        let room_id = self.create_room_with_params(&user.token, body);
        (user, room_id)
    }

    /// Try to find a batch in a Response.
    pub fn get_next_batch(response: &Response) -> Batch {
        response
            .json()
            .get("next_batch")
            .unwrap()
            .as_str()
            .unwrap()
            .parse()
            .unwrap()
    }

    /// Query sync with query parameter.
    pub fn sync(&self, access_token: &str, options: SyncOptions) -> Response {
        let mut path = if let Some(filter) = &options.filter {
            format!(
                "/_matrix/client/r0/sync?filter={}&access_token={}",
                to_string(filter).unwrap(),
                access_token
            )
        } else {
            format!("/_matrix/client/r0/sync?&access_token={}", access_token)
        };
        path = if options.full_state {
            format!("{}&full_state=true", path)
        } else {
            path
        };
        path = match options.set_presence {
            Some(PresenceState::Offline) => format!("{}&set_presence=offline", path),
            Some(PresenceState::Online) => format!("{}&set_presence=online", path),
            Some(PresenceState::Unavailable) => format!("{}&set_presence=unavailable", path),
            None => path,
        };
        path = match options.since {
            Some(batch) => format!("{}&since={}", path, batch.to_string()),
            None => path,
        };
        path = format!("{}&timeout={}", path, options.timeout);

        let response = self.get(&path);
        assert_eq!(response.status, Status::Ok);
        response
    }

    /// Test existent of keys in json.
    pub fn assert_json_keys(json: &Value, keys: Vec<&str>) {
        for key in keys.into_iter() {
            assert!(json.get(key).is_some());
        }
    }

    /// Update presence of a user.
    pub fn update_presence(&self, access_token: &str, user_id: &str, body: &str) -> Response {
        let presence_status_path = format!(
            "/_matrix/client/r0/presence/{}/status?access_token={}",
            user_id, access_token
        );
        let response = self.put(&presence_status_path, body);
        assert_eq!(response.status, Status::Ok);
        response
    }

    /// Set pusher
    pub fn set_pusher(&self, access_token: &str, options: PusherOptions) -> Response {
        let post_pusher = format!(
            "/_matrix/client/r0/pushers/set?access_token={}",
            access_token,
        );

        self.post(&post_pusher, &to_string(&options).unwrap())
    }
}

impl Default for Test {
    fn default() -> Self {
        Self::new()
    }
}

impl Response {
    /// Creates a `Response` from an `iron::response::Response`.
    pub fn from_iron_response(response: iron::response::Response) -> Self {
        let headers = response.headers.clone();
        let status = response.status.expect("Response had no status");
        let body = response::extract_body_to_string(response);

        let json = match from_str(&body) {
            Ok(json) => Some(json),
            _ => None,
        };

        Self {
            body,
            headers,
            json,
            status,
        }
    }

    /// Returns the JSON in the response as a `serde_json::Value`. Panics if response body is not
    /// JSON.
    pub fn json(&self) -> &Value {
        self.json.as_ref().expect("Response did not contain JSON")
    }
}
