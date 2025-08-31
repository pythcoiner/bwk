use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy)]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<LogLevel> for log::LevelFilter {
    fn from(value: LogLevel) -> Self {
        match value {
            LogLevel::Off => Self::Off,
            LogLevel::Error => Self::Error,
            LogLevel::Warn => Self::Warn,
            LogLevel::Info => Self::Info,
            LogLevel::Debug => Self::Debug,
            LogLevel::Trace => Self::Trace,
        }
    }
}

impl From<log::LevelFilter> for LogLevel {
    fn from(value: log::LevelFilter) -> Self {
        match value {
            log::LevelFilter::Off => Self::Off,
            log::LevelFilter::Error => Self::Error,
            log::LevelFilter::Warn => Self::Warn,
            log::LevelFilter::Info => Self::Info,
            log::LevelFilter::Debug => Self::Debug,
            log::LevelFilter::Trace => Self::Trace,
        }
    }
}

#[derive(Debug)]
pub struct Logger {
    level: LogLevel,
    levels: BTreeMap<String, LogLevel>,
}

impl Logger {
    pub fn new(level: LogLevel) -> Self {
        Self {
            level,
            levels: Default::default(),
        }
    }

    pub fn module(&mut self, module: String, level: LogLevel) {
        self.levels.insert(module, level);
    }

    pub fn init(&mut self) {
        let mut logger = env_logger::builder();
        logger.filter_level(self.level.into());
        for (module, lvl) in self.levels.clone() {
            logger.filter_module(&module, lvl.into());
        }
        logger.init();
    }
}

pub fn new_logger(level: LogLevel) -> Box<Logger> {
    Box::new(Logger::new(level))
}
