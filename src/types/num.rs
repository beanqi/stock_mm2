use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Px(pub Decimal);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Qty(pub Decimal);

impl Px {
    pub fn new(v: Decimal) -> Self {
        Self(v)
    }

    pub fn from_str_exact(s: &str) -> Result<Self, rust_decimal::Error> {
        Ok(Self(s.parse()?))
    }

    pub fn is_positive(self) -> bool {
        self.0 > Decimal::ZERO
    }

    pub fn mid(bid: Self, ask: Self) -> Self {
        Self((bid.0 + ask.0) / Decimal::from(2))
    }

    pub fn abs(self) -> Self {
        Self(self.0.abs())
    }
}

impl Qty {
    pub fn new(v: Decimal) -> Self {
        Self(v)
    }

    pub fn from_str_exact(s: &str) -> Result<Self, rust_decimal::Error> {
        Ok(Self(s.parse()?))
    }

    pub fn is_zero(self) -> bool {
        self.0.is_zero()
    }

    pub fn is_positive(self) -> bool {
        self.0 > Decimal::ZERO
    }

    pub fn abs(self) -> Self {
        Self(self.0.abs())
    }

    pub fn signum(self) -> i32 {
        if self.0 > Decimal::ZERO {
            1
        } else if self.0 < Decimal::ZERO {
            -1
        } else {
            0
        }
    }
}

impl fmt::Display for Px {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl fmt::Display for Qty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

macro_rules! impl_arith {
    ($t:ty) => {
        impl Add for $t {
            type Output = Self;
            fn add(self, rhs: Self) -> Self::Output {
                Self(self.0 + rhs.0)
            }
        }
        impl Sub for $t {
            type Output = Self;
            fn sub(self, rhs: Self) -> Self::Output {
                Self(self.0 - rhs.0)
            }
        }
        impl AddAssign for $t {
            fn add_assign(&mut self, rhs: Self) {
                self.0 += rhs.0;
            }
        }
        impl SubAssign for $t {
            fn sub_assign(&mut self, rhs: Self) {
                self.0 -= rhs.0;
            }
        }
        impl Neg for $t {
            type Output = Self;
            fn neg(self) -> Self::Output {
                Self(-self.0)
            }
        }
        impl Mul<Decimal> for $t {
            type Output = Self;
            fn mul(self, rhs: Decimal) -> Self::Output {
                Self(self.0 * rhs)
            }
        }
        impl Div<Decimal> for $t {
            type Output = Self;
            fn div(self, rhs: Decimal) -> Self::Output {
                Self(self.0 / rhs)
            }
        }
    };
}

impl_arith!(Px);
impl_arith!(Qty);

impl From<Decimal> for Px {
    fn from(v: Decimal) -> Self {
        Self(v)
    }
}

impl From<Decimal> for Qty {
    fn from(v: Decimal) -> Self {
        Self(v)
    }
}
