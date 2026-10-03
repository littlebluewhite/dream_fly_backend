//! Pins every wire enum's serde spelling to its `as_str` (== the PG label):
//! `to_value(v) == json!(v.as_str())` for every variant of `ALL`, and, for
//! enums that also deserialize, the value round-trips. DTOs may then carry
//! these enums instead of `String` without changing a byte on the wire.

use serde_json::{json, to_value};

use dream_fly_backend::modules::attendance::model::AttendanceStatus;
use dream_fly_backend::modules::bookings::model::BookingStatus;
use dream_fly_backend::modules::cart::model::CartItemType;
use dream_fly_backend::modules::contact::model::{InquiryStatus, InquiryType};
use dream_fly_backend::modules::courses::model::CourseLevel;
use dream_fly_backend::modules::enrolments::model::EnrolmentStatus;
use dream_fly_backend::modules::leave::model::LeaveStatus;
use dream_fly_backend::modules::notifications::model::NotificationType;
use dream_fly_backend::modules::orders::model::OrderStatus;
use dream_fly_backend::modules::points::model::PointReason;
use dream_fly_backend::modules::posts::model::{PostCategory, PostStatus};
use dream_fly_backend::modules::products::model::ProductType;
use dream_fly_backend::modules::schedule::model::SlotStatus;
use dream_fly_backend::modules::sessions::model::SessionStatus;
use dream_fly_backend::modules::subscriptions::model::SubscriptionStatus;
use dream_fly_backend::modules::waitlist::model::WaitlistStatus;

/// Serialize-only enums.
macro_rules! serialize_spelling {
    ($name:ident, $ty:ty) => {
        #[test]
        fn $name() {
            for v in <$ty>::ALL {
                assert_eq!(to_value(&v).unwrap(), json!(v.as_str()), "{}", v.as_str());
            }
        }
    };
}

/// Enums that serialize and deserialize: also round-trip every variant.
macro_rules! round_trip_spelling {
    ($name:ident, $ty:ty) => {
        #[test]
        fn $name() {
            for v in <$ty>::ALL {
                let json = to_value(&v).unwrap();
                assert_eq!(json, json!(v.as_str()), "{}", v.as_str());
                let back: $ty = serde_json::from_value(json).unwrap();
                assert_eq!(back.as_str(), v.as_str());
            }
        }
    };
}

round_trip_spelling!(attendance_status_serde_matches_as_str, AttendanceStatus);
round_trip_spelling!(booking_status_serde_matches_as_str, BookingStatus);
round_trip_spelling!(cart_item_type_serde_matches_as_str, CartItemType);
round_trip_spelling!(inquiry_status_serde_matches_as_str, InquiryStatus);
round_trip_spelling!(course_level_serde_matches_as_str, CourseLevel);
round_trip_spelling!(leave_status_serde_matches_as_str, LeaveStatus);
round_trip_spelling!(notification_type_serde_matches_as_str, NotificationType);
round_trip_spelling!(order_status_serde_matches_as_str, OrderStatus);
round_trip_spelling!(post_category_serde_matches_as_str, PostCategory);
round_trip_spelling!(post_status_serde_matches_as_str, PostStatus);
round_trip_spelling!(product_type_serde_matches_as_str, ProductType);

serialize_spelling!(enrolment_status_serde_matches_as_str, EnrolmentStatus);
serialize_spelling!(point_reason_serde_matches_as_str, PointReason);
serialize_spelling!(subscription_status_serde_matches_as_str, SubscriptionStatus);
serialize_spelling!(waitlist_status_serde_matches_as_str, WaitlistStatus);
serialize_spelling!(session_status_serde_matches_as_str, SessionStatus);
serialize_spelling!(slot_status_serde_matches_as_str, SlotStatus);
serialize_spelling!(inquiry_type_serde_matches_as_str, InquiryType);
