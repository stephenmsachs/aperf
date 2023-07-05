extern crate ctor;
extern crate lazy_static;

use anyhow::Result;
use crate::data::{CollectData, Data, ProcessedData, DataType, TimeEnum};
use crate::{PERFORMANCE_DATA, PDError, VISUALIZATION_DATA};
use crate::visualizer::{DataVisualizer, GetData};
use chrono::prelude::*;
use ctor::ctor;
use log::{error, trace};
use procfs::process::all_processes;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::collections::HashMap;
use std::sync::Mutex;

pub static PROCESS_FILE_NAME: &str = "processes";

lazy_static! {
    pub static ref TICKS_PER_SECOND: Mutex<u64> = Mutex::new(0);
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProcessesRaw {
    pub time: TimeEnum,
    pub ticks_per_second: u64,
    pub data: String,
}

impl ProcessesRaw {
    pub fn new() -> Self {
        ProcessesRaw {
            time: TimeEnum::DateTime(Utc::now()),
            data: String::new(),
            ticks_per_second: 0,
        }
    }
}

impl CollectData for ProcessesRaw {
    fn prepare_data_collector(&mut self) -> Result<()> {
        *TICKS_PER_SECOND.lock().unwrap() = procfs::ticks_per_second()? as u64;
        Ok(())
    }

    fn collect_data(&mut self) -> Result<()> {
        let ticks_per_second: u64 = *TICKS_PER_SECOND.lock().unwrap();
        self.time = TimeEnum::DateTime(Utc::now());
        self.data = String::new();
        let processes = match all_processes() {
            Err(e) => {
                error!("Failed to read all processes, {}", e);
                return Err(PDError::CollectorAllProcessError.into());
            }
            Ok(p) => p,
        };
        for process in processes {
            let pstat;
            match process.stat() {
                Ok(p) => pstat = p,
                Err(_) => continue,
            };
            let name = pstat.comm;
            let pid = pstat.pid as u64;
            let time_ticks = pstat.utime + pstat.stime;
            let process_entry = format!("{};{};{}\n", name, pid, time_ticks);
            self.data.push_str(&process_entry);
        }
        self.ticks_per_second = ticks_per_second;
        trace!("{:#?}", self.data);
        trace!("{:#?}", self.ticks_per_second);
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProcessTime {
    pub time: TimeEnum,
    pub cpu_time: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Processes {
    pub time: TimeEnum,
    pub entries: Vec<SampleEntry>,
}

impl Processes {
    fn new() -> Self {
        Processes {
            time: TimeEnum::DateTime(Utc::now()),
            entries: Vec::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SampleEntry {
    pub name: String,
    pub pid: u64,
    pub cpu_time: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ProcessEntry {
    pub name: String,
    pub total_cpu_time: u64,
    pub samples: HashMap<TimeEnum, u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EndEntry {
    pub name: String,
    pub total_cpu_time: u64,
    pub entries: Vec<Sample>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct EndEntries {
    pub collection_time: TimeEnum,
    pub end_entries: Vec<EndEntry>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Sample {
    pub cpu_time: u64,
    pub time: TimeEnum,
}

pub fn get_values(values: Vec<Processes>) -> Result<String> {
    let value_zero = values[0].clone();
    let time_zero = value_zero.time;
    let ticks_per_second: u64 = *TICKS_PER_SECOND.lock().unwrap();
    let mut process_map: HashMap<String, ProcessEntry> = HashMap::new();
    let mut total_time: u64 = 1;
    if let TimeEnum::TimeDiff(v) = values.last().unwrap().time - values[0].time {
        if v > 0 {
            total_time = v;
        }
    }

    for value in values {
        for entry in value.entries {
            let time = value.time - time_zero;
            match process_map.get_mut(&entry.name) {
                Some(pe) => {
                    let mut sample_cpu_time: u64 = entry.cpu_time;
                    match pe.samples.get(&time) {
                        Some(v) => {
                            sample_cpu_time += v;
                        },
                        None => {},
                    }
                    pe.samples.insert(time, sample_cpu_time);
                },
                None => {
                    let mut process_entry = ProcessEntry {
                        name: entry.name.clone(),
                        total_cpu_time: 0,
                        samples: HashMap::new(),
                    };
                    process_entry.samples.insert(time, entry.cpu_time);
                    process_map.insert(entry.name, process_entry);
                },
            }
        }
    }
    let mut end_values: EndEntries = EndEntries {
        collection_time: TimeEnum::TimeDiff(total_time),
        end_entries: Vec::new(),
    };

    for (_, process) in process_map.iter_mut() {
        let mut end_entry = EndEntry {
            name: process.name.clone(),
            total_cpu_time: 0,
            entries: Vec::new(),
        };
        let mut entries: Vec<(TimeEnum, u64)> = process.samples.clone().into_iter().collect();
        entries.sort_by(|(a, _), (c, _)| a.cmp(&c));
        let entry_zero: (TimeEnum, u64) = entries[0].clone();
        let mut prev_sample = Sample {time: entry_zero.0, cpu_time: entry_zero.1};
        let mut prev_time: u64 = 0;
        let mut time_now;
        if let TimeEnum::TimeDiff(v) = prev_sample.time {
            prev_time = v;
        }
        for (time, cpu_time) in &entries {
            let sample = Sample {cpu_time: *cpu_time, time: *time};
            /* End sample */
            let mut end_sample = sample.clone();

            if end_sample.cpu_time as i64 - prev_sample.cpu_time as i64 >= 0 {
                /* Update sample based on previous sample */
                end_sample.cpu_time -= prev_sample.cpu_time;
            } else {
                end_sample.cpu_time = 0;
            }
            /* Add to total_cpu_time */
            end_entry.total_cpu_time += end_sample.cpu_time;

            match *time {
                TimeEnum::TimeDiff(v) => {
                    time_now = v;
                    if time_now - prev_time == 0 {
                        continue;
                    }
                }
                _ => continue,
            }

            /* Percentage utilization */
            end_sample.cpu_time /= ticks_per_second * (time_now - prev_time);
            end_sample.cpu_time *= 100;

            prev_time = time_now;
            end_entry.entries.push(end_sample);

            /* Copy to prev_sample */
            prev_sample = sample.clone();
        }
        end_values.end_entries.push(end_entry);
    }
    /* Order the processes by Total CPU Time per collection time */
    end_values.end_entries.sort_by(|a, b| (b.total_cpu_time).cmp(&(a.total_cpu_time)));

    if end_values.end_entries.len() > 16 {
        end_values.end_entries = end_values.end_entries[0..15].to_vec();
    }

    Ok(serde_json::to_string(&end_values)?)
}

impl GetData for Processes {
    fn process_raw_data(&mut self, buffer: Data) -> Result<ProcessedData> {
        let mut processes = Processes::new();
        let raw_value = match buffer {
            Data::ProcessesRaw(ref value) => value,
            _ => panic!("Invalid Data type in raw file"),
        };
        *TICKS_PER_SECOND.lock().unwrap() = raw_value.ticks_per_second as u64;
        let reader = BufReader::new(raw_value.data.as_bytes());
        processes.time = raw_value.time;
        for line in reader.lines() {
            let line = line?;
            let line_str: Vec<&str> = line.split(';').collect();

            let name = line_str[0];
            let pid = line_str[1];
            let cpu_time = line_str[2];
            let sample = SampleEntry {
                name: name.to_string(),
                pid: pid.parse::<u64>()?,
                cpu_time: cpu_time.parse::<u64>()?,
            };
            processes.entries.push(sample);
        }
        let processed_data = ProcessedData::Processes(processes);
        Ok(processed_data)
    }

    fn get_calls(&mut self) -> Result<Vec<String>> {
        let mut end_values = Vec::new();
        end_values.push("values".to_string());
        Ok(end_values)
    }

    fn get_data(&mut self, buffer: Vec<ProcessedData>, query: String) -> Result<String> {
        let mut values = Vec::new();
        for data in buffer {
            match data {
                ProcessedData::Processes(ref value) => values.push(value.clone()),
                _ => unreachable!(),
            }
        }
        let param: Vec<(String, String)> = serde_urlencoded::from_str(&query).unwrap();
        if param.len() < 2 {
            panic!("Not enough arguments");
        }
        let (_, req_str) = &param[1];

        match req_str.as_str() {
            "values" => get_values(values.clone()),
            _ => panic!("Unsupported API"),
        }
    }
}

#[ctor]
fn init_system_processes() {
    let processes_raw = ProcessesRaw::new();
    let file_name = PROCESS_FILE_NAME.to_string();
    let dt = DataType::new(
        Data::ProcessesRaw(processes_raw.clone()),
        file_name.clone(),
        false
    );
    let js_file_name = file_name.clone() + &".js".to_string();
    let processes = Processes::new();
    let dv = DataVisualizer::new(
        ProcessedData::Processes(processes),
        file_name.clone(),
        js_file_name,
        include_str!(concat!(env!("JS_DIR"), "/processes.js")).to_string(),
        file_name.clone(),
    );

    PERFORMANCE_DATA
        .lock()
        .unwrap()
        .add_datatype(file_name.clone(), dt);

    VISUALIZATION_DATA
        .lock()
        .unwrap()
        .add_visualizer(file_name.clone(), dv);
}

#[cfg(test)]
mod process_test {
    use super::{Processes, ProcessesRaw};
    use crate::data::{CollectData, Data, ProcessedData};
    use crate::visualizer::GetData;

    #[test]
    fn test_collect_data() {
        let mut processes = ProcessesRaw::new();
        assert!(processes.prepare_data_collector().unwrap() == ());
        assert!(processes.collect_data().unwrap() == ());
        assert!(!processes.data.is_empty());
    }

    #[test]
    fn test_process_raw_data() {
        let mut buffer: Vec<Data> = Vec::<Data>::new();
        let mut processes_zero = ProcessesRaw::new();
        let mut processes_one = ProcessesRaw::new();
        let mut processed_buffer: Vec<ProcessedData> = Vec::<ProcessedData>::new();

        assert!(processes_zero.prepare_data_collector().unwrap() == ());
        assert!(processes_one.prepare_data_collector().unwrap() == ());
        processes_zero.collect_data().unwrap();
        processes_one.collect_data().unwrap();

        buffer.push(Data::ProcessesRaw(processes_zero));
        buffer.push(Data::ProcessesRaw(processes_one));
        for buf in buffer {
            processed_buffer.push(Processes::new().process_raw_data(buf).unwrap());
        }
        assert!(processed_buffer.len() > 0, "{:#?}", processed_buffer);
    }
}
