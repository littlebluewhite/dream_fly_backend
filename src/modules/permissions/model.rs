/// Closed role set — the value domain of `roles.name` (seeded by the init
/// migration; there is no endpoint that creates roles). `AuthUser.roles`
/// stays `Vec<String>` (wire and access-cache shape); code compares against
/// it through `as_str`. ADR-0014.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Admin,
    Coach,
    Member,
    Guest,
}

impl Role {
    /// Every variant — single owner of the value domain;
    /// `role_all_matches_roles_table` (tests/wire_enums.rs) pins it to the
    /// `roles` rows.
    pub const ALL: [Self; 4] = [Self::Admin, Self::Coach, Self::Member, Self::Guest];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Coach => "coach",
            Self::Member => "member",
            Self::Guest => "guest",
        }
    }
}

impl std::str::FromStr for Role {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|v| v.as_str() == s).ok_or(())
    }
}
