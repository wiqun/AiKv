//! Redis `math.random` / `math.randomseed`. 用 Redis `lrand48` 同一套线性同余.

use std::sync::Mutex;

use mlua::{Lua, MultiValue, Value};

use crate::error::{Error, Result};

const MASK: u32 = 0xFFFF;

struct Lcg {
    x: [u32; 3],
    a: [u32; 3],
    c: u32,
}

impl Lcg {
    fn seed(&mut self, seedval: i32) {
        let seed = seedval as u32;
        self.x = [0x330E, seed & MASK, (seed >> 16) & MASK];
        self.a = [0xE66D, 0xDEEC, 0x5];
        self.c = 0xB;
    }

    fn next(&mut self) {
        let mut p = mul(self.a[0], self.x[0]);
        let carry0 = addequ(&mut p[0], self.c);
        let carry1 = addequ(&mut p[1], carry0);
        let q = mul(self.a[0], self.x[1]);
        let carry0 = addequ(&mut p[1], q[0]);
        let r = mul(self.a[1], self.x[0]);
        let sum = carry0 as u64
            + carry1 as u64
            + carry(p[1], r[0]) as u64
            + q[1] as u64
            + r[1] as u64
            + self.a[0] as u64 * self.x[2] as u64
            + self.a[1] as u64 * self.x[1] as u64
            + self.a[2] as u64 * self.x[0] as u64;
        self.x[2] = (sum as u32) & MASK;
        self.x[1] = (p[1] + r[0]) & MASK;
        self.x[0] = p[0] & MASK;
    }

    fn lrand48(&mut self) -> i32 {
        self.next();
        ((self.x[2] << 15) + (self.x[1] >> 1)) as i32
    }

    fn unit(&mut self) -> f64 {
        let r = self.lrand48() as u32;
        r as f64 / 2147483648.0
    }
}

fn mul(x: u32, y: u32) -> [u32; 2] {
    let product = x as u64 * y as u64;
    [(product as u32) & MASK, ((product >> 16) as u32) & MASK]
}

fn carry(x: u32, y: u32) -> u32 {
    u32::from(x as u64 + y as u64 > MASK as u64)
}

fn addequ(x: &mut u32, y: u32) -> u32 {
    let z = carry(*x, y);
    *x = (*x + y) & MASK;
    z
}

static RNG: Mutex<Lcg> = Mutex::new(Lcg {
    x: [0x330E, 0xABCD, 0x1234],
    a: [0xE66D, 0xDEEC, 0x5],
    c: 0xB,
});

pub fn install(lua: &Lua) -> Result<()> {
    let math: mlua::Table = lua
        .globals()
        .get("math")
        .map_err(|e| Error::Command(format!("ERR sandbox setup failed: {e}")))?;
    let map_err = |e: mlua::Error| Error::Command(format!("ERR sandbox setup failed: {e}"));
    math.set(
        "random",
        lua.create_function(|_, args: MultiValue| random(args))
            .map_err(map_err)?,
    )
    .map_err(map_err)?;
    math.set(
        "randomseed",
        lua.create_function(|_, args: MultiValue| {
            let seed = match args.into_iter().next() {
                Some(v) => number_of(v)? as i32,
                None => 0,
            };
            RNG.lock().expect("math random").seed(seed);
            Ok(())
        })
        .map_err(map_err)?,
    )
    .map_err(map_err)?;
    Ok(())
}

fn random(args: MultiValue) -> mlua::Result<f64> {
    let mut args = args.into_iter();
    let mut rng = RNG.lock().expect("math random");
    let r = rng.unit();
    match (args.next(), args.next()) {
        (None, _) => Ok(r),
        (Some(upper), None) => {
            let u = number_of(upper)? as i64;
            if u < 1 {
                return Err(mlua::Error::runtime("interval is empty"));
            }
            Ok((r * u as f64).floor() + 1.0)
        }
        (Some(lower), Some(upper)) => {
            let l = number_of(lower)? as i64;
            let u = number_of(upper)? as i64;
            if l > u {
                return Err(mlua::Error::runtime("interval is empty"));
            }
            Ok((r * (u - l + 1) as f64).floor() + l as f64)
        }
    }
}

fn number_of(value: Value) -> mlua::Result<f64> {
    match value {
        Value::Integer(i) => Ok(i as f64),
        Value::Number(n) => Ok(n),
        Value::String(s) => std::str::from_utf8(s.as_bytes().as_ref())
            .ok()
            .and_then(|t| t.trim().parse().ok())
            .ok_or_else(|| mlua::Error::runtime("number expected")),
        _ => Err(mlua::Error::runtime("number expected")),
    }
}
