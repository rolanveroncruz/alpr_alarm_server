use std::path::PathBuf;

use axum::{
    body::Bytes,
    extract::State,
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    db_path: PathBuf,
    out_dir: PathBuf,
}


#[derive(Debug, Deserialize)]
struct AlprPayload {
    camid: String,
    date: String,
    plate: String,
    plate_image: String,
    image: String,
}




/// Start the alarm server.
pub async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from("out");

    // -----------------------------------------
    // 1. Create the output directory if necessary.
    // -----------------------------------------
    std::fs::create_dir_all(&out_dir)?;

    let db_path = out_dir.join("alpr_events.db");

    // ------------------------
    // 2. Initialize the database.
    // ------------------------
    initialize_database(&db_path)?;

    let state = AppState {
        db_path,
        out_dir,
    };

    // -----------------------
    // 3. Setup axum server
    // -----------------------

    let app = Router::new()
        .route("/api/alpr/event", post(alpr_webhook_handler))
        .route( "/api/alpr/events", get(get_alpr_events))
        .fallback(fallback_handler)
        .with_state(state);

    println!("Alarm server listening on http://127.0.0.1:3000");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;

    axum::serve(listener, app).await?;

    Ok(())
}


/// Create the database and tables if they don't already exist.
fn initialize_database(
    db_path: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let connection = Connection::open(db_path)?;

    connection.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS alpr_events (
            id TEXT PRIMARY KEY NOT NULL,
            camid TEXT NOT NULL,
            date TEXT NOT NULL,
            plate TEXT NOT NULL,
            image TEXT NOT NULL,
            plate_image TEXT NOT NULL
        );
        "#,
    )?;

    Ok(())
}


/// Receive an ALPR event from the camera.
async fn alpr_webhook_handler(
    State(state): State<AppState>,
    _headers: header::HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    println!("\n=== Incoming ALPR Trigger from VIGI ===");

    // ---------------------------
    // 1. Decode the JSON payload.
    // ---------------------------

    let payload: AlprPayload = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(error) => {
            eprintln!("Failed to parse ALPR JSON: {}", error);
            return StatusCode::BAD_REQUEST;
        }
    };

    println!("Camera: {}", payload.camid);
    println!("Plate:  {}", payload.plate);
    println!("Date:   {}", payload.date);


    // ---------------------------------
    // 2. Open a connection with sqlite
    // ---------------------------------
    let connection = match Connection::open(&state.db_path) {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("Failed to open database: {}", error);
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    // --------------------------------------
    // 3. Generate a unique ID for this event
    // --------------------------------------
    let event_id = Uuid::new_v4().to_string();


    // ----------------------------------------------
    // 4. Decode and save the event images into files.
    // ----------------------------------------------

    let (image_filename, plate_filename) = match decode_and_save_images_to_files(&state, &event_id, &payload.image, &payload.plate_image){
        Ok(filenames)=>filenames,
        Err(error) => {
            eprintln!("Failed to save  images:{}", error);
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    };

    // ------------------------------------
    // 5. Insert the event into SQLite
    // ------------------------------------
    let result = connection.execute(
        r#"
        INSERT INTO alpr_events
            (id,camid, date, plate, image, plate_image)
        VALUES
            (?1, ?2, ?3, ?4, ?5, ?6)
        "#,
        params![
            event_id,
            payload.camid,
            payload.date,
            payload.plate,
            image_filename,
            plate_filename,

        ],
    );
    if let Err(error) = result{
        eprintln!("Failed to insert alpr event: {}", error);
        let _ = std::fs::remove_file(state.out_dir.join(&image_filename));
        let _ = std::fs::remove_file(state.out_dir.join(&plate_filename));
        return StatusCode::INTERNAL_SERVER_ERROR;
    }

    StatusCode::OK
}

fn decode_and_save_images_to_files(state:&AppState,
                                   event_id: &str,
                                   payload_image: &str,
                                   plate_image:  &str,
)->Result<(String, String), std::io::Error>{
    // ------------------------
    // 1. Decode the scene image.
    // -------------------------

    let image_bytes = match STANDARD.decode(payload_image) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("Failed to decode scene image: {}", error);
            return Err(std::io::Error::new(std::io::ErrorKind::Other, error));

        }
    };

    // ------------------------
    // 2.Decode the plate image.
    // ------------------------
    let plate_image_bytes = match STANDARD.decode(plate_image) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("Failed to decode plate image: {}", error);
            return Err(std::io::Error::new(std::io::ErrorKind::Other, error));
        }
    };

    //------------------
    // 3. Set the relevant variables.
    //-------------------
    let image_filename = format!("event_{}.jpg", event_id);
    let plate_image_filename = format!("event_{}_plate.jpg", event_id);

    let image_path = state.out_dir.join(&image_filename);
    let plate_image_path = state.out_dir.join(&plate_image_filename);

    //------------------
    // 4. Save scene image.
    //-------------------
    if let Err(error) = std::fs::write(&image_path, image_bytes) {
        eprintln!("Failed to save scene image: {}", error);
        return Err(std::io::Error::new(std::io::ErrorKind::Other, error));
    }

    // -----------------
    // 5. Save plate image.
    //------------------
    if let Err(error) = std::fs::write(&plate_image_path, plate_image_bytes) {
        eprintln!("Failed to save plate image: {}", error);

        // ✅ Don't leave an orphaned scene image.
        let _ = std::fs::remove_file(&image_path);

        return Err(std::io::Error::new(std::io::ErrorKind::Other, error));
    }
    Ok((image_filename, plate_image_filename))

}

#[derive(Debug, Serialize)]
struct AlprRecord {
    id: String,
    camid: String,
    date: String,
    plate: String,
    image: String,
    plate_image: String,
}
/// Return all ALPR events as a JSON array.
async fn get_alpr_events(
    State(state): State<AppState>,
) -> Result<Json<Vec<AlprRecord>>, StatusCode> {
    let connection = Connection::open(&state.db_path)
        .map_err(|error| {
            eprintln!("Failed to open database: {}", error);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut statement = connection
        .prepare(
            r#"
            SELECT
                id,
                camid,
                date,
                plate,
                image,
                plate_image
            FROM alpr_events
            ORDER BY date DESC
            "#,
        )
        .map_err(|error| {
            eprintln!("Failed to prepare query: {}", error);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let rows = statement
        .query_map([], |row| {
            Ok(AlprRecord {
                id: row.get(0)?,
                camid: row.get(1)?,
                date: row.get(2)?,
                plate: row.get(3)?,
                image: row.get(4)?,
                plate_image: row.get(5)?,
            })
        })
        .map_err(|error| {
            eprintln!("Failed to query ALPR events: {}", error);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let mut records = Vec::new();

    for row in rows {
        records.push(row.map_err(|error| {
            eprintln!("Failed to read ALPR row: {}", error);
            StatusCode::INTERNAL_SERVER_ERROR
        })?);
    }

    Ok(Json(records))
}


/// Temporary handler for requests that don't match a known route.
async fn fallback_handler(
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: header::HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    println!("\n=== UNKNOWN REQUEST ===");
    println!("Method: {}", method);
    println!("URI: {}", uri);
    println!("Body: {} bytes", body.len());

    for (name, value) in &headers {
        if let Ok(value) = value.to_str() {
            println!("{}: {}", name, value);
        }
    }

    StatusCode::NOT_FOUND
}