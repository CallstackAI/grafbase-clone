mod order_by_direction;
use indoc::indoc;
pub use order_by_direction::OrderByDirection;

pub struct DynaqlEnums;

impl DynaqlEnums {
    pub fn sdl() -> String {
        OrderByDirection::sdl()
    }
}

pub trait DynaqlEnum {
    fn ty() -> &'static str;
    fn values() -> Vec<String>;
    fn sdl() -> String {
        format!(
            indoc! {r#"
                enum {ty} {{
                    {values}
                }}
            "#},
            ty = Self::ty(),
            values = Self::values().join("\n    ")
        )
    }
}
