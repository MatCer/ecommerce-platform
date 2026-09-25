//! Tenant staff management. Authorization and audit run in the mutation transaction.
use crate::{audit, tenancy::Role, unique_violation};
use chrono::{DateTime, Utc};
use platform::{Error, db::TenantTx};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StaffMember {
    pub id: Uuid,
    pub user_id: String,
    pub email: String,
    pub role: Role,
    pub created_at: DateTime<Utc>,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    pub email: String,
    pub role: Role,
}
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RoleChange {
    pub role: Role,
}

pub fn authorize(actor: Role, current: Option<Role>, next: Option<Role>) -> Result<(), Error> {
    if actor < Role::Admin
        || (actor != Role::Owner && (current == Some(Role::Owner) || next == Some(Role::Owner)))
    {
        return Err(Error::Forbidden {
            code: "insufficient_role",
        });
    }
    Ok(())
}

pub fn normalize_email(raw: &str) -> Result<String, Error> {
    let email = raw.trim().to_lowercase();
    let valid = email.split_once('@').is_some_and(|(local, domain)| {
        !local.is_empty()
            && !domain.contains('@')
            && domain
                .split_once('.')
                .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
    });
    if email.len() > 254 || email.chars().any(|c| c.is_whitespace() || c.is_control()) || !valid {
        return Err(Error::Validation {
            code: "invalid_email",
            detail: "email must have the form x@y.z and at most 254 bytes".into(),
        });
    }
    Ok(email)
}
fn role_name(role: Role) -> &'static str {
    match role {
        Role::Owner => "owner",
        Role::Admin => "admin",
        Role::Staff => "staff",
    }
}
fn parse_role(role: &str) -> Result<Role, Error> {
    Role::parse(role).ok_or_else(|| Error::Internal("invalid stored staff role".into()))
}
async fn actor_role(tx: &mut TenantTx, actor: &str) -> Result<Role, Error> {
    let role = sqlx::query_scalar!("SELECT role FROM staff_members WHERE user_id = $1", actor)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(Error::Forbidden {
            code: "not_a_member",
        })?;
    parse_role(&role)
}

pub async fn list(tx: &mut TenantTx, actor: &str) -> Result<Vec<StaffMember>, Error> {
    authorize(actor_role(tx, actor).await?, None, None)?;
    sqlx::query!(
        "SELECT id, user_id, email, role, created_at FROM staff_members ORDER BY created_at, id"
    )
    .fetch_all(&mut **tx)
    .await?
    .into_iter()
    .map(|r| {
        Ok(StaffMember {
            id: r.id,
            user_id: r.user_id,
            email: r.email,
            role: parse_role(&r.role)?,
            created_at: r.created_at,
        })
    })
    .collect()
}

// All mutations take the same ordered locks before reading authorization or targets.
// A separate count below gets a fresh READ COMMITTED snapshot after any lock wait.
async fn lock_owners(tx: &mut TenantTx) -> Result<(), Error> {
    sqlx::query!("SELECT id FROM staff_members WHERE role = 'owner' ORDER BY id FOR UPDATE")
        .fetch_all(&mut **tx)
        .await?;
    Ok(())
}
async fn member(tx: &mut TenantTx, id: Uuid) -> Result<StaffMember, Error> {
    let r = sqlx::query!(
        "SELECT id, user_id, email, role, created_at FROM staff_members WHERE id = $1 FOR UPDATE",
        id
    )
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(Error::NotFound)?;
    Ok(StaffMember {
        id: r.id,
        user_id: r.user_id,
        email: r.email,
        role: parse_role(&r.role)?,
        created_at: r.created_at,
    })
}
async fn keep_owner(tx: &mut TenantTx, current: Role, next: Option<Role>) -> Result<(), Error> {
    if current == Role::Owner && next != Some(Role::Owner) {
        let count = sqlx::query_scalar!("SELECT count(*) FROM staff_members WHERE role = 'owner'")
            .fetch_one(&mut **tx)
            .await?
            .unwrap_or(0);
        if count <= 1 {
            return Err(Error::Conflict {
                code: "last_owner",
                detail: "tenant must retain an owner".into(),
            });
        }
    }
    Ok(())
}

pub async fn invite(
    tx: &mut TenantTx,
    actor: &str,
    user_id: &str,
    email: &str,
    role: Role,
) -> Result<StaffMember, Error> {
    lock_owners(tx).await?;
    authorize(actor_role(tx, actor).await?, None, Some(role))?;
    let email = normalize_email(email)?;
    let tenant = tx.tenant_id();
    let stored_role = role_name(role);
    let r = sqlx::query!("INSERT INTO staff_members (tenant_id, user_id, email, role) VALUES ($1, $2, $3, $4) RETURNING id, created_at", tenant, user_id, email, stored_role)
        .fetch_one(&mut **tx).await.map_err(|e| if unique_violation(&e) { Error::Conflict { code: "already_member", detail: "user is already a tenant member".into() } } else { e.into() })?;
    audit::record(
        tx,
        actor,
        "staff.invited",
        "staff_member",
        Some(&r.id.to_string()),
        &json!({"email": email, "role": role}),
    )
    .await?;
    Ok(StaffMember {
        id: r.id,
        user_id: user_id.into(),
        email,
        role,
        created_at: r.created_at,
    })
}
pub async fn change_role(
    tx: &mut TenantTx,
    actor: &str,
    id: Uuid,
    role: Role,
) -> Result<StaffMember, Error> {
    lock_owners(tx).await?;
    let mut target = member(tx, id).await?;
    authorize(actor_role(tx, actor).await?, Some(target.role), Some(role))?;
    keep_owner(tx, target.role, Some(role)).await?;
    let stored_role = role_name(role);
    sqlx::query!(
        "UPDATE staff_members SET role = $2 WHERE id = $1",
        id,
        stored_role
    )
    .execute(&mut **tx)
    .await?;
    audit::record(
        tx,
        actor,
        "staff.role_changed",
        "staff_member",
        Some(&id.to_string()),
        &json!({"from": target.role, "to": role}),
    )
    .await?;
    target.role = role;
    Ok(target)
}
pub async fn remove(tx: &mut TenantTx, actor: &str, id: Uuid) -> Result<(), Error> {
    lock_owners(tx).await?;
    let target = member(tx, id).await?;
    authorize(actor_role(tx, actor).await?, Some(target.role), None)?;
    keep_owner(tx, target.role, None).await?;
    sqlx::query!("DELETE FROM staff_members WHERE id = $1", id)
        .execute(&mut **tx)
        .await?;
    audit::record(
        tx,
        actor,
        "staff.removed",
        "staff_member",
        Some(&id.to_string()),
        &json!({"email": target.email, "role": target.role}),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authorization_matrix() {
        for actor in [Role::Staff, Role::Admin, Role::Owner] {
            for current in [
                None,
                Some(Role::Staff),
                Some(Role::Admin),
                Some(Role::Owner),
            ] {
                for next in [
                    None,
                    Some(Role::Staff),
                    Some(Role::Admin),
                    Some(Role::Owner),
                ] {
                    assert_eq!(
                        authorize(actor, current, next).is_ok(),
                        actor == Role::Owner
                            || (actor == Role::Admin
                                && current != Some(Role::Owner)
                                && next != Some(Role::Owner))
                    );
                }
            }
        }
    }
    #[test]
    fn emails_are_normalized_and_validated() {
        assert_eq!(
            normalize_email("  A@Example.COM ").ok().as_deref(),
            Some("a@example.com")
        );
        for email in [
            "",
            "x",
            "@x.y",
            "x@y",
            "x@.z",
            "x@y.",
            "x@@y.z",
            "x y@a.b",
            &format!("{}@a.b", "x".repeat(251)),
        ] {
            assert_eq!(
                normalize_email(email).expect_err("invalid email").code(),
                "invalid_email"
            );
        }
    }
}
