//! Endpoints for joining rooms.

use bodyparser;
use diesel::pg::PgConnection;
use diesel::prelude::*;
use iron::status::Status;
use iron::{Chain, Handler, IronResult, Plugin, Request, Response};
use ruma_identifiers::{RoomId, RoomIdOrAliasId, UserId};

use crate::config::Config;
use crate::db::DB;
use crate::error::ApiError;
use crate::middleware::{
    AccessTokenAuth, JsonRequest, MiddlewareChain, RoomIdOrAliasParam, RoomIdParam,
};
use crate::models::room::Room;
use crate::models::room_alias::RoomAlias;
use crate::models::room_membership::{RoomMembership, RoomMembershipOptions};
use crate::models::user::User;
use crate::modifier::{EmptyResponse, SerializableResponse};

/// The `/rooms/:room_id/join` endpoint.
#[derive(Clone, Copy, Debug)]
pub struct JoinRoom;

/// The body of the response for this API.
#[derive(Debug, Serialize)]
struct JoinRoomResponse {
    /// The joined room.
    room_id: RoomId,
}

middleware_chain!(JoinRoom, [JsonRequest, RoomIdParam, AccessTokenAuth]);

impl Handler for JoinRoom {
    fn handle(&self, request: &mut Request<'_, '_>) -> IronResult<Response> {
        let user = request
            .extensions
            .get::<User>()
            .expect("AccessTokenAuth should ensure a user")
            .clone();

        let connection = DB::from_request(request)?;
        let config = Config::from_request(request)?;

        let room_id = request
            .extensions
            .get::<RoomIdParam>()
            .expect("Should have been required by RoomIdParam.")
            .clone();

        join_room(room_id, user, &connection, &config)
    }
}

/// The `/join/:room_id_or_alias` endpoint.
#[derive(Clone, Copy, Debug)]
pub struct JoinRoomWithIdOrAlias;

middleware_chain!(
    JoinRoomWithIdOrAlias,
    [JsonRequest, RoomIdOrAliasParam, AccessTokenAuth]
);

impl Handler for JoinRoomWithIdOrAlias {
    fn handle(&self, request: &mut Request<'_, '_>) -> IronResult<Response> {
        let user = request
            .extensions
            .get::<User>()
            .expect("AccessTokenAuth should ensure a user")
            .clone();

        let connection = DB::from_request(request)?;
        let config = Config::from_request(request)?;

        let room_id_or_alias = request
            .extensions
            .get::<RoomIdOrAliasParam>()
            .expect("Should have been required by RoomIdOrAliasParam.")
            .clone();

        let room_id = match room_id_or_alias {
            RoomIdOrAliasId::RoomId(id) => id,
            RoomIdOrAliasId::RoomAliasId(alias) => {
                let room_alias = RoomAlias::find_by_alias(&connection, &alias)?;
                room_alias.room_id
            }
        };

        join_room(room_id, user, &connection, &config)
    }
}

/// Handles the work of actually saving the user to the room membership table
fn join_room(
    room_id: RoomId,
    user: User,
    connection: &PgConnection,
    config: &Config,
) -> IronResult<Response> {
    let room_membership_options = RoomMembershipOptions {
        room_id: room_id.clone(),
        user_id: user.id.clone(),
        sender: user.id,
        membership: "join".to_string(),
    };

    let room_membership =
        RoomMembership::upsert(connection, &config.domain, room_membership_options)?;

    let response = JoinRoomResponse {
        room_id: room_membership.room_id,
    };

    Ok(Response::with((Status::Ok, SerializableResponse(response))))
}

/// The `/rooms/:room_id/leave` endpoint.
#[derive(Clone, Copy, Debug)]
pub struct LeaveRoom;

middleware_chain!(LeaveRoom, [JsonRequest, RoomIdParam, AccessTokenAuth]);

impl Handler for LeaveRoom {
    fn handle(&self, request: &mut Request<'_, '_>) -> IronResult<Response> {
        let user = request
            .extensions
            .get::<User>()
            .expect("AccessTokenAuth should ensure a user")
            .clone();

        let connection = DB::from_request(request)?;
        let config = Config::from_request(request)?;

        let room_id = request
            .extensions
            .get::<RoomIdParam>()
            .expect("Should have been required by RoomIdParam.")
            .clone();

        let room_membership_options = RoomMembershipOptions {
            room_id: room_id.clone(),
            user_id: user.id.clone(),
            sender: user.id.clone(),
            membership: "leave".to_string(),
        };

        if Room::find(&connection, &room_id)?.is_none() {
            Err(ApiError::unauthorized(
                "The room was not found on this server".to_string(),
            ))?;
        }

        match RoomMembership::find(&connection, &room_id, &user.id)? {
            Some(mut room_membership) => match room_membership.membership.as_str() {
                "leave" => Ok(Response::with(Status::Ok)),
                "join" | "invite" => {
                    room_membership.update(&connection, &config.domain, room_membership_options)?;
                    Ok(Response::with(EmptyResponse(Status::Ok)))
                }
                "ban" => Err(ApiError::unauthorized(
                    "User is banned from the room".to_string(),
                ))?,
                _ => Err(ApiError::unauthorized(
                    "Invalid membership state".to_string(),
                ))?,
            },
            None => Err(ApiError::unauthorized(
                "User not in room or uninvited".to_string(),
            ))?,
        }
    }
}

/// The `/rooms/:room_id/kick` endpoint.
#[derive(Clone, Copy, Debug)]
pub struct KickFromRoom;

/// The body of the request for this API.
#[derive(Clone, Debug, Deserialize)]
struct KickFromRoomRequest {
    /// The reason the user has been kicked.
    pub reason: Option<String>,
    /// The fully qualified user ID of the user being kicked.
    pub user_id: UserId,
}

middleware_chain!(KickFromRoom, [JsonRequest, RoomIdParam, AccessTokenAuth]);

impl Handler for KickFromRoom {
    fn handle(&self, request: &mut Request<'_, '_>) -> IronResult<Response> {
        let room_id = request
            .extensions
            .get::<RoomIdParam>()
            .expect("RoomIdParam should ensure a room_id")
            .clone();

        let kicker = request
            .extensions
            .get::<User>()
            .expect("AccessTokenAuth should ensure a user")
            .clone();

        let kickee_id = match request.get::<bodyparser::Struct<KickFromRoomRequest>>() {
            Ok(Some(req)) => req.user_id,
            Ok(None) => Err(ApiError::bad_json(None))?,
            Err(err) => Err(ApiError::bad_json(err.to_string()))?,
        };

        let connection = DB::from_request(request)?;
        let config = Config::from_request(request)?;

        let room = match Room::find(&connection, &room_id)? {
            Some(room) => room,
            None => Err(ApiError::unauthorized(
                "The room was not found on this server".to_string(),
            ))?,
        };

        match RoomMembership::find(&connection, &room_id, &kicker.id)? {
            Some(ref membership) if membership.membership == "join" => {}
            _ => Err(ApiError::unauthorized(
                "The kicker is not currently in the room".to_string(),
            ))?,
        };

        let mut kickee_membership = match RoomMembership::find(&connection, &room_id, &kickee_id)? {
            Some(ref membership) if membership.membership == "join" => membership.clone(),
            _ => Err(ApiError::unauthorized(
                "The kickee is not currently in the room".to_string(),
            ))?,
        };

        let power_levels = room.current_power_levels(&connection)?;
        let user_power_level = power_levels
            .users
            .get(&kicker.id)
            .unwrap_or(&power_levels.users_default);

        if power_levels.kick > *user_power_level {
            Err(ApiError::unauthorized(
                "Insufficient power level to kick a user".to_string(),
            ))?;
        }

        let room_membership_options = RoomMembershipOptions {
            room_id,
            user_id: kickee_id,
            sender: kicker.id,
            membership: "leave".to_string(),
        };

        kickee_membership.update(&connection, &config.domain, room_membership_options)?;

        Ok(Response::with(EmptyResponse(Status::Ok)))
    }
}

/// The `/rooms/:room_id/invite` endpoint.
#[derive(Clone, Copy, Debug)]
pub struct InviteToRoom;

/// The body of the request for this API.
#[derive(Clone, Debug, Deserialize)]
struct InviteToRoomRequest {
    /// The fully qualified user ID of the invitee.
    pub user_id: UserId,
}

middleware_chain!(InviteToRoom, [JsonRequest, RoomIdParam, AccessTokenAuth]);

impl Handler for InviteToRoom {
    fn handle(&self, request: &mut Request<'_, '_>) -> IronResult<Response> {
        let room_id = request
            .extensions
            .get::<RoomIdParam>()
            .expect("RoomIdParam should ensure a room_id")
            .clone();

        let inviter = request
            .extensions
            .get::<User>()
            .expect("AccessTokenAuth should ensure a user")
            .clone();

        let invitee_id = match request.get::<bodyparser::Struct<InviteToRoomRequest>>() {
            Ok(Some(req)) => req.user_id,
            Ok(None) => Err(ApiError::missing_param("user_id"))?,
            Err(err) => Err(ApiError::bad_json(err.to_string()))?,
        };

        let connection = DB::from_request(request)?;
        let config = Config::from_request(request)?;

        let invitee_membership = connection
            .transaction::<Option<RoomMembership>, ApiError, _>(|| {
                if User::find_active_user(&connection, &invitee_id)?.is_none() {
                    return Err(ApiError::not_found(format!(
                        "The invited user {} was not found on this server",
                        invitee_id
                    )));
                }

                if Room::find(&connection, &room_id)?.is_none() {
                    return Err(ApiError::unauthorized(
                        "The room was not found on this server".to_string(),
                    ));
                }

                let unauthorized_err =
                    ApiError::unauthorized("The inviter hasn't joined the room yet".to_string());

                // Check if the inviter has joined the room.
                RoomMembership::find(&connection, &room_id, &inviter.id).and_then(
                    |membership| match membership {
                        Some(entry) => match entry.membership.as_ref() {
                            "join" => Ok(()),
                            _ => Err(unauthorized_err),
                        },
                        None => Err(unauthorized_err),
                    },
                )?;

                let membership = RoomMembership::find(&connection, &room_id, &invitee_id)?;

                Ok(membership)
            })
            .map_err(ApiError::from)?;

        let new_membership_options = RoomMembershipOptions {
            room_id,
            user_id: invitee_id,
            sender: inviter.id,
            membership: "invite".to_string(),
        };

        if let Some(mut entry) = invitee_membership {
            match entry.membership.as_ref() {
                "invite" => {}
                "ban" => Err(ApiError::unauthorized(
                    "The invited user is banned from the room".to_string(),
                ))?,
                "join" => Err(ApiError::unauthorized(
                    "The invited user has already joined".to_string(),
                ))?,
                _ => {
                    entry.update(&connection, &config.domain, new_membership_options)?;
                }
            }
        } else {
            RoomMembership::create(&connection, &config.domain, new_membership_options)?;
        }

        Ok(Response::with(EmptyResponse(Status::Ok)))
    }
}

#[cfg(test)]
mod tests {
    use crate::test::Test;
    use iron::status::Status;

    #[test]
    fn join_own_public_room_via_join_endpoint() {
        let test = Test::new();
        let user = test.create_user();
        let room_id = test.create_public_room(&user.token);

        let room_join_path = format!(
            "/_matrix/client/r0/join/{}?access_token={}",
            room_id, user.token
        );

        let response = test.post(&room_join_path, r"{}");
        assert_eq!(response.status, Status::Ok);
        assert_eq!(
            response
                .json()
                .get("room_id")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
            room_id
        );
    }

    #[test]
    fn join_own_public_room_via_join_endpoint_alias() {
        let test = Test::new();
        let user = test.create_user();
        let room_id = test.create_room_with_params(
            &user.token,
            r#"{"room_alias_name":"thepub", "visibility": "public"}"#,
        );

        let room_join_path = format!(
            "/_matrix/client/r0/join/{}?access_token={}",
            "%23thepub:ruma.test", // Hash symbols need to be urlencoded
            user.token
        );

        let response = test.post(&room_join_path, r"{}");
        assert_eq!(response.status, Status::Ok);
        assert_eq!(
            response
                .json()
                .get("room_id")
                .unwrap()
                .as_str()
                .unwrap()
                .to_string(),
            room_id
        );
    }

    #[test]
    fn join_own_public_room() {
        let test = Test::new();
        let (carl, room_id) = test.initial_fixtures(r#"{"visibility": "public"}"#);

        let room_join_path = format!(
            "/_matrix/client/r0/rooms/{}/join?access_token={}",
            room_id, carl.token
        );

        let response = test.post(&room_join_path, r"{}");

        assert_eq!(response.status, Status::Ok);
        assert!(response.json().get("room_id").unwrap().as_str().is_some());
    }

    #[test]
    fn join_other_public_room() {
        let test = Test::new();
        let (_, room_id) = test.initial_fixtures(r#"{"visibility": "public"}"#);
        let mark = test.create_user();

        let room_join_path = format!(
            "/_matrix/client/r0/rooms/{}/join?access_token={}",
            room_id, mark.token
        );

        let response = test.post(&room_join_path, r"{}");

        assert_eq!(response.status, Status::Ok);
        assert!(response.json().get("room_id").unwrap().as_str().is_some());
    }

    #[test]
    fn join_own_private_room() {
        let test = Test::new();
        let (carl, room_id) = test.initial_fixtures(r#"{"visibility": "private"}"#);

        let room_join_path = format!(
            "/_matrix/client/r0/rooms/{}/join?access_token={}",
            room_id, carl.token
        );

        let response = test.post(&room_join_path, r"{}");

        assert_eq!(response.status, Status::Ok);
        assert!(response.json().get("room_id").unwrap().as_str().is_some());
    }

    #[test]
    fn join_other_private_room() {
        let test = Test::new();
        let carl = test.create_user();
        let mark = test.create_user();

        let body = format!(r#"{{"visibility": "private", "invite": ["{}"]}}"#, mark.id);
        let room_id = test.create_room_with_params(&carl.token, &body);

        let room_join_path = format!(
            "/_matrix/client/r0/rooms/{}/join?access_token={}",
            room_id, mark.token
        );

        let response = test.post(&room_join_path, r"{}");

        assert_eq!(response.status, Status::Ok);
        assert!(response.json().get("room_id").unwrap().as_str().is_some());
    }

    #[test]
    fn join_other_private_room_without_invite() {
        let test = Test::new();
        let (_, room_id) = test.initial_fixtures(r#"{"visibility": "private"}"#);
        let alice = test.create_user();

        let room_join_path = format!(
            "/_matrix/client/r0/rooms/{}/join?access_token={}",
            room_id, alice.token
        );

        let response = test.post(&room_join_path, r"{}");

        assert_eq!(response.status, Status::Forbidden);
    }

    #[test]
    fn invite_to_room() {
        let test = Test::new();
        let (bob, room_id) = test.initial_fixtures(r#"{"visibility": "private"}"#);
        let alice = test.create_user();

        let response = test.invite(&bob.token, &room_id, &alice.id);

        assert_eq!(response.status, Status::Ok);

        assert!(test.join_room(&alice.token, &room_id).status.is_success());
    }

    #[test]
    fn invite_before_joining() {
        let test = Test::new();

        let carl = test.create_user();
        let bob = test.create_user();
        let alice = test.create_user();

        // Carl invites Bob.
        let body = format!(r#"{{"visibility": "private", "invite": ["{}"]}}"#, bob.id);
        let room_id = test.create_room_with_params(&carl.token, &body);

        // Bob invites Alice before joining.
        let response = test.invite(&bob.token, &room_id, &alice.id);

        assert_eq!(response.status, Status::Forbidden);
        assert_eq!(
            response.json().get("error").unwrap().as_str().unwrap(),
            "The inviter hasn't joined the room yet"
        );
    }

    #[test]
    fn invite_without_user_id() {
        let test = Test::new();
        let (carl, room_id) = test.initial_fixtures(r#"{"visibility": "private"}"#);

        let invite_path = format!(
            "/_matrix/client/r0/rooms/{}/invite?access_token={}",
            room_id, carl.token
        );

        // Empty body.
        let response = test.post(&invite_path, "{}");

        assert_eq!(response.status, Status::UnprocessableEntity);
    }

    #[test]
    fn invitee_does_not_exist() {
        let test = Test::new();
        let (carl, room_id) = test.initial_fixtures(r#"{"visibility": "private"}"#);

        // User 'mark' does not exist.
        let response = test.invite(&carl.token, &room_id, "@mark:ruma.test");

        assert_eq!(response.status, Status::NotFound);
        assert_eq!(
            response.json().get("error").unwrap().as_str().unwrap(),
            "The invited user @mark:ruma.test was not found on this server"
        );
    }

    #[test]
    fn invitee_is_invalid() {
        let test = Test::new();
        let (carl, room_id) = test.initial_fixtures(r#"{"visibility": "private"}"#);

        let response = test.invite(&carl.token, &room_id, "mark.ruma.test");

        assert_eq!(response.status, Status::UnprocessableEntity);
    }

    #[test]
    fn invitee_is_already_invited() {
        let test = Test::new();
        let bob = test.create_user();
        let alice = test.create_user();

        let room_id = test.create_room_with_params(
            &bob.token,
            format!(r#"{{"visibility": "private", "invite": ["{}"]}}"#, alice.id).as_str(),
        );

        let response = test.invite(&bob.token, &room_id, &alice.id);

        assert_eq!(response.status, Status::Ok);
    }

    #[test]
    fn invitee_has_already_joined() {
        let test = Test::new();
        let bob = test.create_user();
        let alice = test.create_user();

        let room_id = test.create_room_with_params(
            &bob.token,
            format!(r#"{{"visibility": "private", "invite": ["{}"]}}"#, alice.id).as_str(),
        );

        assert!(test.join_room(&alice.token, &room_id).status.is_success());

        let response = test.invite(&bob.token, &room_id, &alice.id);

        assert_eq!(response.status, Status::Forbidden);
        assert_eq!(
            response.json().get("error").unwrap().as_str().unwrap(),
            "The invited user has already joined"
        );
    }

    #[test]
    fn room_does_not_exist() {
        let test = Test::new();
        let bob = test.create_user();
        let alice = test.create_user();

        let response = test.invite(&bob.token, "!random:ruma.test", &alice.id);

        assert_eq!(response.status, Status::Forbidden);
        assert_eq!(
            response.json().get("error").unwrap().as_str().unwrap(),
            "The room was not found on this server"
        );
    }

    #[test]
    fn leave_own_room() {
        let test = Test::new();
        let (alice, room_id) = test.initial_fixtures(r#"{"visibility": "private"}"#);

        let leave_room_path = format!(
            "/_matrix/client/r0/rooms/{}/leave?access_token={}",
            room_id, alice.token
        );

        let response = test.post(&leave_room_path, r#"{}"#);
        assert_eq!(response.status, Status::Ok);

        let response = test.send_message(&alice.token, &room_id, "Hi", 1);
        assert_eq!(response.status, Status::Forbidden);
        assert_eq!(
            response.json().get("error").unwrap().as_str().unwrap(),
            format!("The user {} has not joined the room", alice.id)
        );
    }

    #[test]
    fn leave_nonexistent_room() {
        let test = Test::new();
        let alice = test.create_user();

        let leave_room_path = format!(
            "/_matrix/client/r0/rooms/{}/leave?access_token={}",
            "!random_room_id:ruma.test", alice.token,
        );

        let response = test.post(&leave_room_path, r#"{}"#);
        assert_eq!(response.status, Status::Forbidden);
        assert_eq!(
            response.json().get("error").unwrap().as_str().unwrap(),
            "The room was not found on this server"
        );
    }

    #[test]
    fn leave_uninvited_room() {
        let test = Test::new();
        let bob = test.create_user();
        let (_, room_id) = test.initial_fixtures(r#"{"visibility": "public"}"#);

        let leave_room_path = format!(
            "/_matrix/client/r0/rooms/{}/leave?access_token={}",
            room_id, bob.token,
        );

        let response = test.post(&leave_room_path, r#"{}"#);
        assert_eq!(response.status, Status::Forbidden);
        assert_eq!(
            response.json().get("error").unwrap().as_str().unwrap(),
            "User not in room or uninvited"
        );
    }

    #[test]
    fn leave_invited_room() {
        let test = Test::new();
        let bob = test.create_user();
        let (alice, room_id) = test.initial_fixtures(r#"{"visibility": "private"}"#);

        let response = test.invite(&alice.token, &room_id, &bob.id);
        assert_eq!(response.status, Status::Ok);

        let leave_room_path = format!(
            "/_matrix/client/r0/rooms/{}/leave?access_token={}",
            room_id, bob.token,
        );

        let response = test.post(&leave_room_path, r#"{}"#);
        assert_eq!(response.status, Status::Ok);

        let response = test.send_message(&bob.token, &room_id, "Hi", 1);
        assert_eq!(response.status, Status::Forbidden);
        assert_eq!(
            response.json().get("error").unwrap().as_str().unwrap(),
            format!("The user {} has not joined the room", bob.id)
        );
    }

    #[test]
    fn leave_joined_room() {
        let test = Test::new();
        let bob = test.create_user();
        let (alice, room_id) = test.initial_fixtures(r#"{"visibility": "private"}"#);

        let response = test.invite(&alice.token, &room_id, &bob.id);
        assert_eq!(response.status, Status::Ok);

        let response = test.join_room(&bob.token, &room_id);
        assert_eq!(response.status, Status::Ok);

        let leave_room_path = format!(
            "/_matrix/client/r0/rooms/{}/leave?access_token={}",
            room_id, bob.token,
        );

        let response = test.post(&leave_room_path, r#"{}"#);
        assert_eq!(response.status, Status::Ok);

        let response = test.send_message(&bob.token, &room_id, "Hi", 1);
        assert_eq!(response.status, Status::Forbidden);
        assert_eq!(
            response.json().get("error").unwrap().as_str().unwrap(),
            format!("The user {} has not joined the room", bob.id)
        );
    }

    #[test]
    fn kick_user() {
        let test = Test::new();
        let alice = test.create_user();
        let bob = test.create_user();

        let room_options = format!(r#"{{"invite": ["{}"]}}"#, bob.id);
        let room_id = test.create_room_with_params(&alice.token, &room_options);

        assert_eq!(test.join_room(&bob.token, &room_id).status, Status::Ok);

        assert_eq!(
            test.send_message(&bob.token, &room_id, "Hi", 1).status,
            Status::Ok
        );

        assert_eq!(
            test.kick_from_room(&alice.token, &room_id, &bob.id, None)
                .status,
            Status::Ok
        );

        let response = test.send_message(&bob.token, &room_id, "Hi", 2);

        assert_eq!(
            response.json().get("error").unwrap().as_str().unwrap(),
            format!("The user {} has not joined the room", &bob.id)
        );
        assert_eq!(response.status, Status::Forbidden);
    }

    #[test]
    fn kick_user_without_permissions() {
        let test = Test::new();
        let alice = test.create_user();
        let bob = test.create_user();

        let room_options = format!(r#"{{"invite": ["{}"]}}"#, bob.id);
        let room_id = test.create_room_with_params(&alice.token, &room_options);

        assert_eq!(test.join_room(&bob.token, &room_id).status, Status::Ok);

        assert_eq!(
            test.send_message(&bob.token, &room_id, "Hi", 1).status,
            Status::Ok
        );

        let response = test.kick_from_room(&bob.token, &room_id, &alice.id, None);

        assert_eq!(response.status, Status::Forbidden);
        assert_eq!(
            response.json().get("error").unwrap().as_str().unwrap(),
            "Insufficient power level to kick a user"
        );
    }

    #[test]
    fn kick_user_from_invalid_room() {
        let test = Test::new();
        let alice = test.create_user();
        let bob = test.create_user();

        let room_options = format!(r#"{{"invite": ["{}"]}}"#, bob.id);
        let room_id = test.create_room_with_params(&alice.token, &room_options);

        assert_eq!(test.join_room(&bob.token, &room_id).status, Status::Ok);

        assert_eq!(
            test.send_message(&bob.token, &room_id, "Hi", 1).status,
            Status::Ok
        );

        let response = test.kick_from_room(&alice.token, "!invalid_room:ruma.test", &bob.id, None);

        assert_eq!(response.status, Status::Forbidden);
        assert_eq!(
            response.json().get("error").unwrap().as_str().unwrap(),
            "The room was not found on this server"
        );
    }

    #[test]
    fn kicker_not_in_room() {
        let test = Test::new();
        let alice = test.create_user();
        let bob = test.create_user();
        let carl = test.create_user();

        let room_options = format!(r#"{{"invite": ["{}"]}}"#, bob.id);
        let room_id = test.create_room_with_params(&alice.token, &room_options);

        let response = test.kick_from_room(&bob.token, &room_id, &carl.id, None);

        assert_eq!(response.status, Status::Forbidden);
        assert_eq!(
            response.json().get("error").unwrap().as_str().unwrap(),
            "The kicker is not currently in the room"
        );
    }

    #[test]
    fn kickee_not_in_room() {
        let test = Test::new();
        let alice = test.create_user();
        let bob = test.create_user();

        let room_options = format!(r#"{{"invite": ["{}"]}}"#, bob.id);
        let room_id = test.create_room_with_params(&alice.token, &room_options);

        let response = test.kick_from_room(&alice.token, &room_id, &bob.id, None);

        assert_eq!(response.status, Status::Forbidden);
        assert_eq!(
            response.json().get("error").unwrap().as_str().unwrap(),
            "The kickee is not currently in the room"
        );
    }
}
