#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use std::{fs, io::{Cursor, Read}, num::NonZeroU32, path::PathBuf};
use std::process::Command;
use std::process::Stdio;

use caesium::{compress_in_memory, parameters::CSParameters};
use iced::{Alignment, Element, Length, Size, Task, widget::{Container, PickList, Space, Text, button, column, container, row, rule}};
use iced_aw::Spinner;
use image::imageops;
use include_dir::{Dir, include_dir};
use log::{LevelFilter, error};
use log4rs::{append::rolling_file::{RollingFileAppender, policy::compound::{CompoundPolicy, roll::fixed_window::FixedWindowRoller, trigger::onstartup::OnStartUpTrigger}}, config::{Appender, Root}, encode::pattern::PatternEncoder};
use pdfium_render::prelude::{PdfPageObjectsCommon, PdfPagePaperSize, PdfPoints, Pdfium, PdfiumError};
use printers::common::base::{job::PrinterJobOptions, printer::Printer};
use smartcrop::ResizableImage;
use strum::{Display, EnumIter};
#[cfg(target_os="windows")]
const NAPS2: Dir<'_> = include_dir!("./resources/naps2");
#[cfg(target_os="windows")]
const PDFIUM: &[u8] = include_bytes!("../resources/pdfium/pdfium.dll");
#[cfg(target_os="linux")]
const PDFIUM: &[u8] = include_bytes!("../resources/pdfium/libpdfium.so");

fn main() -> iced::Result {
    init_logger();
    init_pdfium();
    #[cfg(target_os="windows")]
    init_naps2();

    iced::application(State::new, State::update, State::view)
        .title("ID Copy")
        .theme(iced::Theme::CatppuccinMacchiato)
        .window_size(Size::new(600.0, 400.0))
        .run()
}

fn init_logger() {
    let roller = FixedWindowRoller::builder()
        .build(format!("./logs/archive_{{}}.log").as_str(), 3)
        .unwrap();
    // let trigger = SizeTrigger::new(10 * 1024 * 1024);
    let trigger = OnStartUpTrigger::new(0);
    let policy = CompoundPolicy::new(Box::new(trigger), Box::new(roller));
    let logfile = RollingFileAppender::builder()
        .encoder(Box::new(PatternEncoder::new("{d(%Y-%m-%d %H:%M:%S)} {M} {L} [{l}] {m}\n")))
        .build(format!("./logs/latest.log"), Box::new(policy))
        .unwrap();

    let log_config = log4rs::Config::builder()
        .appender(Appender::builder().build("logfile", Box::new(logfile)))
        .build(Root::builder()
            .appender("logfile")
            .build(LevelFilter::Warn))
        .unwrap();

    log4rs::init_config(log_config).unwrap();
}

fn init_pdfium() {
    if cfg!(target_os = "windows") {
        if !fs::exists("./pdfium.dll").unwrap() {
            match fs::write("./pdfium.dll", &PDFIUM) {
                Err(err) => {
                    error!("Error writing Pdfium bytes to file: {}", err);
                },
                _ => {}
            }
        }
    }
    else if cfg!(target_os = "linux") {
        if !fs::exists("./libpdfium.so").unwrap() {
            match fs::write("./libpdfium.so", &PDFIUM) {
                Err(err) => {
                    error!("Error writing Pdfium bytes to file: {}", err);
                },
                _ => {}
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn init_naps2() {
    if !fs::exists("./naps2").unwrap() {
        match fs::create_dir("./naps2") {
            Err(err) => {
                error!("Error creating NAPS2 directory: {}", err);
            },
            _ => {}
        }
        match NAPS2.extract("./naps2") {
            Err(err) => {
                error!("Error extracting NAPS2 files to directory: {}", err);
            },
            _ => {}
        }
    }
}

struct State {
    scanner_list: Vec<String>,
    printer_list: Vec<String>,
    selected_scanner: Option<String>,
    selected_printer: Option<String>,
    scanned_id: Option<(Vec<u8>, Vec<u8>)>,
    pdf_path: Option<PathBuf>,
    fetching_scanners: bool,
    fetching_printers: bool,
    scanning: bool,
    printing: bool,
    print_copies: u8,
    paper_size: PaperSize
}

impl State {
    fn new() -> (State, Task<Message>) {
        (
            State {
                scanner_list: Vec::new(),
                printer_list: Vec::new(),
                selected_scanner: None,
                selected_printer: None,
                scanned_id: None,
                pdf_path: None,
                fetching_scanners: true,
                fetching_printers: true,
                scanning: false,
                printing: false,
                print_copies: 1,
                paper_size: PaperSize::default()
            },
            Task::batch(
                vec![
                    Task::perform(fetch_scanners(), |result| {
                        match result {
                            Ok(scanners) => {
                                Message::ScannersFetched(scanners)
                            },
                            Err(_) => {
                                Message::ScannerFetchFail
                            }
                        }
                    }),
                    Task::perform(fetch_printers(), Message::PrintersFetched)
                ]
            )
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::SelectScanner(scanner) => {
                self.selected_scanner = Some(scanner);
                Task::none()
            },
            Message::ScannersFetched(scanners) => {
                self.fetching_scanners = false;
                self.scanner_list = scanners;
                if self.scanner_list.len() > 0 {
                    self.selected_scanner = Some(self.scanner_list[0].clone());
                }
                Task::none()
            },
            Message::ScannerFetchFail => {
                self.fetching_scanners = false;
                Task::none()
            },
            Message::SelectPrinter(printer) => {
                self.selected_printer = Some(printer);
                Task::none()
            },
            Message::PrintersFetched(printers) => {
                self.fetching_printers = false;
                self.printer_list = printers;
                Task::none()
            },
            Message::Scan => {
                self.scanning = true;
                Task::perform(
                    run_scan(self.selected_scanner.as_ref().unwrap().to_string()),
                    move |result| {
                        match result {
                            Ok(paths) => {
                                Message::ScanComplete(paths)
                            },
                            Err(err) => {
                                error!("Error scanning: {}", err);
                                Message::ScanFail
                            }
                        }
                    }
                )
            },
            Message::ScanComplete(paths) => {
                self.scanning = false;
                let mut front: Vec<u8> = Vec::new();
                let mut back: Vec<u8> = Vec::new();
                match fs::read(&paths.0) {
                    Ok(bytes) => {
                        front = bytes;
                    },
                    Err(err) => {
                        error!("Error reading scanned data: {}", err);
                    }
                }
                match fs::read(&paths.1) {
                    Ok(bytes) => {
                        back = bytes;
                    },
                    Err(err) => {
                        error!("Error reading scanned data: {}", err);
                    }
                }
                self.scanned_id = Some((front, back));
                match create_pdf(self.scanned_id.as_ref().unwrap().clone()) {
                    Err(err) => {
                        error!("Error creating PDF: {}", err);
                    },
                    _ => {}
                }
                match opener::open("./scanned.pdf") {
                    Err(err) => {
                        error!("Error opening created PDF: {}", err);
                    },
                    _ => {}
                }
                match fs::remove_file("./scan-01.png") {
                    Err(err) => {
                        error!("Error deleting temp scan image 1: {}", err);
                    },
                    _ => {}
                }
                match fs::remove_file("./scan-02.png") {
                    Err(err) => {
                        error!("Error deleting temp scan image 2: {}", err);
                    },
                    _ => {}
                }
                Task::none()
            },
            Message::ScanFail => {
                self.scanning = false;
                Task::none()
            },
            Message::Print => {
                match create_pdf(self.scanned_id.as_ref().unwrap().clone()) {
                    Err(err) => {
                        error!("Error creating PDF: {}", err);
                    },
                    _ => {}
                }
                match opener::open("./scanned.pdf") {
                    Err(err) => {
                        error!("Error opening created PDF: {}", err);
                    },
                    _ => {}
                }
                // let printer = printers::get_printer_by_name(self.selected_printer.as_ref().unwrap());
                // let print_copies = self.print_copies.clone().to_string();
                // let properties = [
                //     ("copies", print_copies.as_str()),
                //     ("media", self.paper_size.to_printer_args())

                // ];
                // match printer {
                //     Some(printer) => {
                //         match printer.print(&self.pdf_output.as_ref().unwrap(), PrinterJobOptions {
                //             name: Some("ID Copy"),
                //             raw_properties: &properties,
                //             converter: printers::common::converters::Converter::None
                //         }) {
                //             Err(err) => {
                //                 error!("Error printing: {}", err.message);
                //             },
                //             _ => {}
                //         };
                //     },
                //     None => {
                //         error!("No printer was found");
                //     }
                // }
                Task::none()
            }
        }
    }

    fn view(&'_ self) -> Element<'_, Message> {
        Container::new(
            row![
                column![
                    Text::new("ID Copy").size(20),
                    rule::horizontal(2),
                    Text::new("Scanner:"),
                    row![
                        PickList::new(self.scanner_list.clone(), self.selected_scanner.clone(), Message::SelectScanner),
                        if self.fetching_scanners {
                            Container::new(Spinner::new())
                        }
                        else {
                            Container::new(Space::new())
                        }
                    ].spacing(5).align_y(Alignment::Center),
                    row![
                        if self.scanning {
                            button(Spinner::new())
                        }
                        else if self.selected_scanner.is_none() {
                            button("Scan")
                        }
                        else {
                            button("Scan").on_press(Message::Scan)
                        },
                    ].spacing(5)
                ].spacing(5)
            ].spacing(5)
        ).padding(5).style(container::bordered_box).width(600).height(400).into()
    }
}

impl Drop for State {
    fn drop(&mut self) {
        match fs::remove_file("./scanned.pdf") {
            Err(err) => {
                error!("Error removing PDF output: {}", err);
            },
            _ => {}
        }
    }
}

#[derive(Clone)]
enum Message {
    SelectScanner(String),
    ScannersFetched(Vec<String>),
    ScannerFetchFail,
    SelectPrinter(String),
    PrintersFetched(Vec<String>),
    Scan,
    ScanComplete((PathBuf, PathBuf)),
    ScanFail,
    Print

}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Display, EnumIter)]
enum PaperSize {
    #[strum(serialize = "A4 (210 x 297 mm)")]
    #[default]
    A4,
    #[strum(serialize = "Letter (8.5 x 11 in)")]
    Letter,
    #[strum(serialize = "Long (8.5 x 13 in)")]
    Long,
    #[strum(serialize = "Legal (8.5 x 14 in)")]
    Legal
}

impl PaperSize {
    fn to_printer_args(&self) -> &str {
        match self {
            PaperSize::A4 => "A4",
            PaperSize::Letter => "Letter",
            PaperSize::Long => "Custom.8.5x13in",
            PaperSize::Legal => "Legal"
        }
    }
}

async fn fetch_scanners() -> Result<Vec<String>, String> {
    #[cfg(target_os="windows")]
    let output = Command::new("./naps2/NAPS2.Console.exe")
        .arg("--listdevices")
        .arg("--driver")
        .arg("wia")
        .output()
        .map_err(|err| err.to_string())?;

    // Requires NAPS2 to be installed
    #[cfg(target_os="linux")]
    let output = Command::new("naps2")
        .arg("console")
        .arg("--listdevices")
        .arg("--driver")
        .arg("sane")
        .output()
        .map_err(|err| err.to_string())?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let list = stdout
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
            Ok(list)
    }
    else {
        Err("Error fetching scanners".to_string())
    }
    
}

async fn fetch_printers() -> Vec<String> {
    let mut printer_list: Vec<String> = Vec::new();
    for printer in printers::get_printers() {
        printer_list.push(printer.name)
    }
    return printer_list
}

async fn run_scan(device: String) -> Result<(PathBuf, PathBuf), String> {
    let temp_path = PathBuf::from("./scan-$(nn).png");
    #[cfg(target_os = "windows")]
    let mut output = Command::new("./naps2/NAPS2.Console.exe")
        .arg("-o")
        .arg(format!("{}", &temp_path.to_string_lossy()))
        .arg("--driver")
        .arg("wia")
        .arg("--device")
        .arg(device)
        .arg("--source")
        .arg("duplex")
        .arg("--pagesize")
        .arg("85.6x54mm")
        .arg("--dpi")
        .arg("300")
        .arg("--bitdepth")
        .arg("color")
        .arg("--deskew")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| err.to_string())?;

    #[cfg(target_os = "linux")]
    let mut output = Command::new("naps2")
        .arg("console")
        .arg("-o")
        .arg(format!("{}", &temp_path.to_string_lossy()))
        .arg("--driver")
        .arg("sane")
        .arg("--device")
        .arg(device)
        .arg("--source")
        .arg("duplex")
        .arg("--pagesize")
        .arg("85.6x54mm")
        .arg("--dpi")
        .arg("300")
        .arg("--bitdepth")
        .arg("color")
        .arg("--deskew")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| err.to_string())?;

    let status = output.wait().map_err(|err| err.to_string())?;

    if !status.success() {
        let mut err_msg = String::new();
        output.stderr.take().unwrap().read_to_string(&mut err_msg).ok();
        return Err(format!("NAPS2 Error: {}", err_msg));
    }

    Ok((PathBuf::from("./scan-01.png"), PathBuf::from("./scan-02.png")))
}

fn create_pdf(scanned_id: (Vec<u8>, Vec<u8>)) -> Result<(), PdfiumError> {
    let pdfium = Pdfium::default();
    let mut document = match pdfium.create_new_pdf() {
        Ok(document) => document,
        Err(err) => {
            error!("Error creating new PDF document: {}", err);
            panic!("Error creating new PDF document: {}", err);
        }
    };

    let mut page = match document.pages_mut().create_page_at_end(PdfPagePaperSize::a4()) {
        Ok(page) => page,
        Err(err) => {
            error!("Error creating PDF page: {}", err);
            panic!("Error creating PDF page: {}", err);
        }
    };
    
    let top_bytes = match image::load_from_memory(&scanned_id.0) {
        Ok(bytes) => {
            let mut converted_bytes: Vec<u8> = Vec::new();
            let res = smartcrop::find_best_crop(&bytes, NonZeroU32::new(1013).unwrap(), NonZeroU32::new(638).unwrap());
            let crop = res.unwrap().crop;
            match bytes.crop_and_resize(crop.clone(), 1013, 638).write_to(&mut Cursor::new(&mut converted_bytes), image::ImageFormat::Png) {
                Err(err) => {
                    error!("Error converting image: {}", err);
                }
                _ => {}
            }

            Some(match image::load_from_memory(&converted_bytes) {
                Ok(image) => image,
                Err(err) => {
                    error!("Error loading converted bytes from memory: {}", err);
                    panic!("Error loading converted bytes from memory: {}", err);
                }
            })
        },
        Err(err) => {
            error!("Error loading image from memory: {}", err);
            panic!("Error loading image from memory: {}", err);
        }
    };

    let bottom_bytes = match image::load_from_memory(&scanned_id.1) {
        Ok(bytes) => {
            let mut converted_bytes: Vec<u8> = Vec::new();
            let res = smartcrop::find_best_crop(&bytes, NonZeroU32::new(1013).unwrap(), NonZeroU32::new(638).unwrap());
            let crop = res.unwrap().crop;
            match bytes.crop_and_resize(crop.clone(), 1013, 638).write_to(&mut Cursor::new(&mut converted_bytes), image::ImageFormat::Png) {
                Err(err) => {
                    error!("Error converting image: {}", err);
                }
                _ => {}
            }

            Some(match image::load_from_memory(&converted_bytes) {
                Ok(image) => image,
                Err(err) => {
                    error!("Error loading converted bytes from memory: {}", err);
                    panic!("Error loading converted bytes from memory: {}", err);
                }
            })
        },
        Err(err) => {
            error!("Error loading image from memory: {}", err);
            panic!("Error loading image from memory: {}", err);
        }
    };

    let top_bytes_width = (top_bytes.as_ref().unwrap().width() / 4) as f32;
    let bottom_bytes_width = (bottom_bytes.as_ref().unwrap().width() / 4) as f32;
    let top_bytes_height = (top_bytes.as_ref().unwrap().height() / 4) as f32;
    let bottom_bytes_height = (bottom_bytes.as_ref().unwrap().height() / 4) as f32;
    
    let gap: f32 = 40.0;
    let y_offset: f32 = 150.0;
    println!("{}, {}", top_bytes_width, top_bytes_height);
    match page.objects_mut().create_image_object((PdfPagePaperSize::a4().width() - (PdfPoints::new(top_bytes_width))) / 2.0, (PdfPagePaperSize::a4().height() / 2.0) + PdfPoints::new(gap) + PdfPoints::new(y_offset), top_bytes.as_ref().unwrap(), Some(PdfPoints::new(top_bytes_width)), Some(PdfPoints::new(top_bytes_height))) {
        Err(err) => {
            error!("Error adding image to PDF page: {}", err);
            panic!("Error adding image to PDF page: {}", err);
        },
        _ => {}
    };
    
    match page.objects_mut().create_image_object((PdfPagePaperSize::a4().width() - (PdfPoints::new(bottom_bytes_width))) / 2.0, (PdfPagePaperSize::a4().height() / 2.0) - PdfPoints::new(bottom_bytes_height) - PdfPoints::new(gap) + PdfPoints::new(y_offset), bottom_bytes.as_ref().unwrap(), Some(PdfPoints::new(bottom_bytes_width)), Some(PdfPoints::new(bottom_bytes_height))) {
        Err(err) => {
            error!("Error adding image to PDF page: {}", err);
            panic!("Error adding image to PDF page: {}", err);
        },
        _ => {}
    };

    match document.save_to_file("./scanned.pdf") {
        Ok(_) => Ok(()),
        Err(err) => {
            error!("Error saving PDF file: {}", err);
            return Err(err)
        }
    }
}