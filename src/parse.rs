use std::{
    fmt::Write as _,
    fs::{create_dir_all, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use log::{debug, info, warn};
use reqwest::{
    blocking::Client,
    header::{COOKIE, USER_AGENT},
};
use serde::{Deserialize, Serialize};
use serde_json::{from_slice, from_value};

use crate::{
    consts::STATION_CODE,
    pyetgr::Station,
    utils::{trans_date, trans_time},
};

/// 获取所有途径线路上的车站的列车时刻
/// https://kyfw.12306.cn/otn/czxx/init API 无法使用导致复杂度上升为 n^2 故尽量不要使用过长的基线
pub fn parse_line(
    path: impl AsRef<Path>,
    date: String,
    http_client: reqwest::blocking::Client,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut data = vec![];
    File::open(path.as_ref())?.read_to_end(&mut data)?;
    info!("加载线路文件成功: {:?}", path.as_ref());
    let stations: Vec<Station> =
        from_value(from_slice::<serde_json::Value>(&data)?["line"]["stations"].clone())?;
    debug!("开始读取Cookie");
    let mut cookie: Vec<u8> = vec![];
    if OpenOptions::new()
        .create(true)
        .write(true)
        .read(true)
        .open(&"./cookie.txt")
        .expect("创建cookie.txt文件失败")
        .read_to_end(&mut cookie)
        .expect("读取失败cookie.txt文件失败")
        == 0
    {
        panic!("读取cookie.txt失败, 文件为空，请填入cookie后再使用");
    }
    let cookie = String::from_utf8(cookie)?;
    debug!("加载Cookie成功");

    let sc_map = &STATION_CODE;
    let mut scs = vec![];
    for station in stations {
        match sc_map.get(&*station.zhanming) {
            Some(code) => scs.push(code.to_string()),
            None => info!("无法找到站点信息:{}, 将跳过此站", station.zhanming),
        }
    }
    let mut composes = vec![];
    for i in 0..scs.len() {
        for j in (i + 1)..scs.len() {
            composes.push((&scs[i], &scs[j]));
            composes.push((&scs[j], &scs[i]))
        }
    }
    // 待查询列车队列
    let mut queue: Vec<String> = vec![];
    for comp in composes {
        debug!("解析车站{} - {}", comp.0, comp.1);
        let resp=http_client
        .get(format!("https://kyfw.12306.cn/otn/leftTicket/query?leftTicketDTO.train_date={}&leftTicketDTO.from_station={}&leftTicketDTO.to_station={}&purpose_codes=ADULT",&trans_date(&date),comp.0,comp.1))
        .header(USER_AGENT, COMMON_HEADER)
        .header(COOKIE,  &cookie)
        .send()?
        .json::<serde_json::Value>()?;

        let train_nos: Vec<String> = serde_json::from_value(resp["data"]["result"].clone())?;
        for trash in train_nos {
            let mut stack = trash.split('|');
            stack.next();
            stack.next();
            stack.next();
            let train_no = stack.next().unwrap().to_owned();
            stack.next();
            stack.next();
            if stack.next().unwrap() == comp.0
                && stack.next().unwrap() == comp.1
                && !queue.contains(&train_no)
            {
                debug!("发现列车: {}", &train_no);
                queue.push(train_no)
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    info!("共解析到{:?}趟列车", queue.len());
    parse_train(date, queue, http_client)?;

    Ok(())
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct TrainData {
    train_date: String,
    start_date: String,
    stop_date: String,
    train_no: String,
    station_no: String,
    station_name: String,
    bureau_code: String,
    station_telecode: String,
    station_train_code: String,
    day_difference: u8,
    arrive_time: String,
    arrive_timestamp: serde_json::Number,
    start_time: String,
    start_timestamp: serde_json::Number,
    ticket_delay: serde_json::Number,
    /// 候车室
    waiting_room: String,
    /// 检票口
    wicket: String,
    distance: serde_json::Number,
    time_span: serde_json::Number,
    one_station_cross_day: bool,
}

static COMMON_HEADER:&str="Mozilla/5.0 (Linux; Android 12; Redmi Note 8 Build/SP2A.220405.004; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/86.0.4240.99 XWEB/3235 MMWEBSDK/20220402 Mobile Safari/537.36 MMWEBID/1660 MicroMessenger/8.0.22.2140(0x28001637) WeChat/arm64 Weixin NetType/WIFI Language/zh_CN ABI/arm64 MiniProgramEnv/android";
/// 构造查询url
#[inline(always)]
fn query_by_train_code(train_code: &str, date: &str) -> String {
    format!("https://wifi.12306.cn/wifiapps/ticket/api/stoptime/queryByTrainCode?trainCode={}&trainDate={}&getBigScreen=true", train_code,date)
}
pub fn parse_train(
    date: String,
    trains: Vec<String>,
    http_client: Client,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut csv = File::create("./output.csv")?;
    let mut trf_buf = String::with_capacity(1024);
    let mut csv_buf = String::with_capacity(1024);
    // 记录历史查询
    let mut train_history = vec![];
    debug!("date:{date}");
    for train_code in trains.iter() {
        // 排除重复
        if train_history.contains(train_code) {
            continue;
        }
        let train_info: serde_json::Value = http_client
            .get(query_by_train_code(train_code, &date))
            .header(USER_AGENT, COMMON_HEADER)
            .send()?
            .json()?;
        if train_info["status"] == serde_json::Value::from(serde_json::Number::from(-1)) {
            warn!("无法查到该趟列车信息: {}", train_code);
            continue;
        }
        let deserialized: Vec<TrainData> =
            serde_json::from_value(train_info["data"].clone()).unwrap();
        if deserialized.is_empty() {
            info!("未查询到 {} 的站点信息", train_code);
        }
        let mut train_number = (String::new(), String::new());
        let banke = "true";

        csv_buf.clear();
        trf_buf.clear();
        info!("开始获取列车时刻表");
        for station in deserialized.iter() {
            debug!(
                "获取到 {} 在 {} 的到达时间为 {}，出发时间为 {}。",
                station.station_train_code,
                station.station_name,
                station.arrive_time,
                station.start_time
            );

            // Fixme:判断最后一个字符
            if train_number.0.is_empty() || train_number.1.is_empty() {
                match (&station.station_train_code)[(station.station_train_code.len() - 1)..]
                    .parse::<u8>()
                    .unwrap()
                {
                    0 | 2 | 4 | 6 | 8 => train_number.1 = station.station_train_code.clone(), //上行
                    _ => train_number.0 = station.station_train_code.clone(),                 //下行
                }
            }
            writeln!(
                csv_buf,
                "{},{},{},{},",
                station.station_train_code,
                station.station_name,
                trans_time(&station.arrive_time),
                trans_time(&station.start_time),
            )?;
            writeln!(
                trf_buf,
                "{},{},{},{},NA,0,",
                station.station_name,
                trans_time(&station.arrive_time),
                trans_time(&station.start_time),
                banke
            )?;
        }
        //todo:timetable处理

        // 写入csv文件
        csv.write_all(csv_buf.as_bytes())?;

        // 写入trf文件
        create_dir_all("./ETRC/").unwrap();
        let mut trf = File::create(format!("./ETRC/{}.trf", train_code))?;
        write!(
            trf,
            "trf2,{},{},{},NA,\n{}\n{}\n{}",
            train_code,
            train_number.0,
            train_number.1,
            &deserialized.first().unwrap().station_name,
            &deserialized.last().unwrap().station_name,
            trf_buf
        )?;
        // 将查询到的列车送入历史记录
        train_history.push(train_number.0);
        train_history.push(train_number.1);
    }
    info!("获取时刻表任务完成");
    Ok(())
}
