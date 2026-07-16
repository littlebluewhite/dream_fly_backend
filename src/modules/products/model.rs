use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "product_type", rename_all = "snake_case")]
pub enum ProductType {
    Ticket,
    CoursePackage,
    Membership,
    Merchandise,
}

impl ProductType {
    /// The SQL string literal for this variant — matches the Postgres
    /// `product_type` enum's `snake_case` spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ticket => "ticket",
            Self::CoursePackage => "course_package",
            Self::Membership => "membership",
            Self::Merchandise => "merchandise",
        }
    }
}

impl std::str::FromStr for ProductType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ticket" => Ok(Self::Ticket),
            "course_package" => Ok(Self::CoursePackage),
            "membership" => Ok(Self::Membership),
            "merchandise" => Ok(Self::Merchandise),
            _ => Err(()),
        }
    }
}

#[derive(Debug, sqlx::FromRow, Serialize)]
pub struct Product {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub product_type: ProductType,
    pub description: Option<String>,
    pub price_cents: i64,
    pub original_price_cents: Option<i64>,
    pub features: Vec<String>,
    pub is_highlighted: bool,
    pub badge: Option<String>,
    pub stock: Option<i32>,
    pub valid_days: Option<i32>,
    pub session_count: Option<i32>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_matches_the_snake_case_sql_spelling() {
        assert_eq!(ProductType::Ticket.as_str(), "ticket");
        assert_eq!(ProductType::CoursePackage.as_str(), "course_package");
        assert_eq!(ProductType::Membership.as_str(), "membership");
        assert_eq!(ProductType::Merchandise.as_str(), "merchandise");
    }

    #[test]
    fn from_str_parses_every_as_str_output_back_to_its_variant() {
        assert!(matches!("ticket".parse::<ProductType>(), Ok(ProductType::Ticket)));
        assert!(matches!(
            "course_package".parse::<ProductType>(),
            Ok(ProductType::CoursePackage)
        ));
        assert!(matches!(
            "membership".parse::<ProductType>(),
            Ok(ProductType::Membership)
        ));
        assert!(matches!(
            "merchandise".parse::<ProductType>(),
            Ok(ProductType::Merchandise)
        ));
    }

    #[test]
    fn as_str_and_from_str_round_trip_for_every_variant() {
        for v in [
            ProductType::Ticket,
            ProductType::CoursePackage,
            ProductType::Membership,
            ProductType::Merchandise,
        ] {
            let s = v.as_str();
            let parsed: ProductType = s.parse().expect("as_str output must parse");
            assert_eq!(parsed.as_str(), s);
        }
    }

    #[test]
    fn from_str_rejects_unknown_value() {
        assert!("bogus".parse::<ProductType>().is_err());
    }
}
