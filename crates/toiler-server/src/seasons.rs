//! 四立交节是全球同一时刻，不受电脑时区或夏令时影响。
//!
//! lunar_rust 的寿星天文历返回北京时间的儒略日，已包含太阳视黄经和 ΔT 修正；
//! 必须减去固定 UTC+8 才能与 SystemTime 比较，不能再按本机时区转换。

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lunar_rust::{
    lunar::Lunar,
    lunar_year::{LunarYear, LunarYearRefHelper},
    solar::{self, SolarRefHelper},
};

/// 运行中应用图标与新建对话插画共用四立边界，避免前后端各算一套。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Season {
    Spring,
    Summer,
    Autumn,
    Winter,
}

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeasonSnapshot {
    pub season: Season,
    pub server_time: u64,
    pub next_change_at: u64,
}

const UNIX_JULIAN_DAY: f64 = 2_440_587.5;
const SECONDS_PER_DAY: f64 = 86_400.0;
const BEIJING_OFFSET_SECONDS: f64 = 8.0 * 3_600.0;
const MAX_CHECK_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct Calendar {
    // 只缓存当前公历年与下一年立春；轮询不必每次重新计算天文历。
    cached: Option<(i64, [(SystemTime, Season); 5])>,
}

impl Calendar {
    pub fn at(&mut self, now: SystemTime) -> Result<(Season, Duration), String> {
        let (season, next_change) = self.resolve(now)?;
        let delay = next_change.duration_since(now).map_err(|e| e.to_string())?;
        Ok((season, delay.min(MAX_CHECK_INTERVAL)))
    }

    pub fn snapshot(&mut self, now: SystemTime) -> Result<SeasonSnapshot, String> {
        let (season, next_change) = self.resolve(now)?;
        let millis = |time: SystemTime| -> Result<u64, String> {
            time.duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_millis()
                .try_into()
                .map_err(|_| "节气时间戳超出支持范围".into())
        };
        Ok(SeasonSnapshot {
            season,
            server_time: millis(now)?,
            next_change_at: millis(next_change)?,
        })
    }

    fn resolve(&mut self, now: SystemTime) -> Result<(Season, SystemTime), String> {
        let elapsed = now.duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?;
        let utc_julian_day = UNIX_JULIAN_DAY + elapsed.as_secs_f64() / SECONDS_PER_DAY;
        // 上游日期构造会把秒四舍五入；取 UTC 当日零点，避免年末最后半秒进位成 12/32。
        let utc_midnight = (utc_julian_day - 0.5).floor() + 0.5;
        let year = solar::from_julian_day(utc_midnight).get_year();
        if self.cached.as_ref().map(|(y, _)| *y) != Some(year) {
            let [spring, summer, autumn, winter] = transitions(year)?;
            let next_spring = transitions(year + 1)?[0];
            self.cached = Some((year, [spring, summer, autumn, winter, next_spring]));
        }
        let events = &self.cached.as_ref().ok_or("节气日历未就绪")?.1;
        // 立春前延续上年冬季；恰好交节时刻起使用新季节（左闭右开）。
        let mut season = Season::Winter;
        for &(instant, next_season) in events {
            if instant > now {
                return Ok((season, instant));
            }
            season = next_season;
        }
        Err("未找到下一次四立交节时刻".into())
    }
}

fn transitions(year: i64) -> Result<[(SystemTime, Season); 4], String> {
    let names = Lunar::get_jie_qi_in_use();
    let dates = LunarYear::from_lunar_year(year).get_jie_qi_julian_days();
    let mut result = [(UNIX_EPOCH, Season::Winter); 4];
    // 中文“四立”属于本公历年；表尾 LI_CHUN 属于下一年，不能混用。
    for (index, (name, season)) in [
        ("立春", Season::Spring),
        ("立夏", Season::Summer),
        ("立秋", Season::Autumn),
        ("立冬", Season::Winter),
    ]
    .into_iter()
    .enumerate()
    {
        let position = names.iter().position(|n| n == name).ok_or("节气名称缺失")?;
        let day = *dates.get(position).ok_or("节气时刻缺失")?;
        // 以算法计算值取最近整秒；秒级分辨率不代表天文模型没有误差。
        let seconds = ((day - UNIX_JULIAN_DAY) * SECONDS_PER_DAY - BEIJING_OFFSET_SECONDS).round();
        if !seconds.is_finite() || seconds < 0.0 {
            return Err("节气时刻超出支持范围".into());
        }
        let instant = UNIX_EPOCH
            .checked_add(Duration::from_secs_f64(seconds))
            .ok_or("节气时刻溢出")?;
        result[index] = (instant, season);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(year: i64, month: i64, day: i64, hour: i64, minute: i64) -> SystemTime {
        let jd = solar::from_ymdhms(year, month, day, hour, minute, 0).get_julian_day();
        UNIX_EPOCH + Duration::from_secs(((jd - UNIX_JULIAN_DAY) * SECONDS_PER_DAY).round() as u64)
    }

    #[test]
    fn matches_published_2024_four_commencements_minutes() {
        // 香港天文台 2024 年年历（四立，北京时间）：
        // 2/4 16:27、5/5 08:10、8/7 08:09、11/7 06:20。
        // https://www.hko.gov.hk/en/gts/astronomy/Solar_Term.htm
        // 公布到分钟，允许一分钟模型/取整误差，并用 UTC 样本防止漏减八小时。
        let expected = [
            utc(2024, 2, 4, 8, 27),
            utc(2024, 5, 5, 0, 10),
            utc(2024, 8, 7, 0, 9),
            utc(2024, 11, 6, 22, 20),
        ];
        for ((actual, _), expected) in transitions(2024).unwrap().into_iter().zip(expected) {
            let difference = actual
                .duration_since(expected)
                .unwrap_or_else(|e| e.duration());
            assert!(
                difference < Duration::from_secs(60),
                "四立交节时间误差：{difference:?}"
            );
        }
    }

    #[test]
    fn switches_at_each_instant_including_leap_year_and_year_rollover() {
        for year in [2024, 2026, 2027, 2030] {
            let mut calendar = Calendar::default();
            let mut previous = Season::Winter;
            for (instant, season) in transitions(year).unwrap() {
                let before = instant - Duration::from_millis(1);
                assert_eq!(
                    calendar.at(before).unwrap(),
                    (previous, Duration::from_millis(1))
                );
                assert_eq!(calendar.at(instant).unwrap().0, season);
                assert_eq!(
                    calendar.at(instant + Duration::from_secs(1)).unwrap().0,
                    season
                );
                previous = season;
            }
            assert_eq!(
                calendar.at(utc(year + 1, 1, 1, 0, 0)).unwrap().0,
                Season::Winter
            );
            assert_eq!(
                calendar
                    .at(utc(year + 1, 1, 1, 0, 0) - Duration::from_millis(1))
                    .unwrap()
                    .0,
                Season::Winter
            );
            // 模拟系统时钟回拨，缓存必须跟随公历年重新计算。
            assert_eq!(
                calendar.at(utc(year, 3, 1, 0, 0)).unwrap().0,
                Season::Spring
            );
            assert_eq!(
                calendar.at(utc(year, 6, 1, 0, 0)).unwrap().0,
                Season::Summer
            );
        }
    }

    #[test]
    fn follows_traditional_seasons_between_four_commencements_and_equinoxes() {
        let mut calendar = Calendar::default();
        for (month, day, season) in [
            (1, 15, Season::Winter),
            (2, 15, Season::Spring),
            (5, 15, Season::Summer),
            (8, 15, Season::Autumn),
            (11, 18, Season::Winter),
        ] {
            assert_eq!(calendar.at(utc(2026, month, day, 0, 0)).unwrap().0, season);
        }
    }

    #[test]
    fn snapshot_exposes_the_actual_next_boundary_instead_of_the_polling_interval() {
        let mut calendar = Calendar::default();
        let events = transitions(2026).unwrap();
        let now = utc(2026, 11, 26, 0, 0);
        let snapshot = calendar.snapshot(now).unwrap();
        assert_eq!(snapshot.season, Season::Winter);
        assert_eq!(
            snapshot.server_time,
            now.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
        );
        assert_eq!(
            snapshot.next_change_at,
            transitions(2027).unwrap()[0]
                .0
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64
        );
        let before = calendar
            .snapshot(events[1].0 - Duration::from_millis(1))
            .unwrap();
        let after = calendar.snapshot(events[1].0).unwrap();
        assert_eq!(before.season, Season::Spring);
        assert_eq!(before.next_change_at, after.server_time);
        assert_eq!(after.season, Season::Summer);
        assert!(after.next_change_at > after.server_time);
    }

    #[test]
    fn polls_at_most_every_minute_but_wakes_at_the_transition() {
        let mut calendar = Calendar::default();
        let spring = transitions(2026).unwrap()[0].0;
        assert_eq!(
            calendar.at(spring - Duration::from_secs(120)).unwrap().1,
            MAX_CHECK_INTERVAL
        );
        assert_eq!(
            calendar.at(spring - Duration::from_secs(7)).unwrap().1,
            Duration::from_secs(7)
        );
        assert_eq!(calendar.at(spring).unwrap().1, MAX_CHECK_INTERVAL);
    }
}
