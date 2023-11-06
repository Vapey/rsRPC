use std::{
  collections::HashMap,
  sync::{Arc, Mutex},
};

use simple_websockets::{Event, EventHub, Message, Responder};

use crate::{cmd::ActivityCmd, logger};

use super::process::ProcessDetectedEvent;

fn empty_activity(pid: u64, socket_id: String) -> String {
  format!(
    r#"
    {{
      "activity": null,
      "pid": {},
      "socketId": "{}"
    }}
  "#,
    pid, socket_id
  )
}

#[derive(Clone)]
pub struct ClientConnector {
  pub port: u16,
  server: Arc<Mutex<EventHub>>,
  pub clients: Arc<Mutex<HashMap<u64, Responder>>>,
  data_on_connect: String,

  pub last_pid: Option<u64>,
  pub last_socket_id: Option<String>,

  pub ipc_event_rec: Arc<Mutex<std::sync::mpsc::Receiver<ActivityCmd>>>,
  pub proc_event_rec: Arc<Mutex<std::sync::mpsc::Receiver<ProcessDetectedEvent>>>,
}

impl ClientConnector {
  pub fn new(
    port: u16,
    data_on_connect: String,
    ipc_event_rec: std::sync::mpsc::Receiver<ActivityCmd>,
    proc_event_rec: std::sync::mpsc::Receiver<ProcessDetectedEvent>,
  ) -> ClientConnector {
    ClientConnector {
      server: Arc::new(Mutex::new(simple_websockets::launch(port).unwrap())),
      clients: Arc::new(Mutex::new(HashMap::new())),
      data_on_connect,
      port,

      last_pid: None,
      last_socket_id: None,

      ipc_event_rec: Arc::new(Mutex::new(ipc_event_rec)),
      proc_event_rec: Arc::new(Mutex::new(proc_event_rec)),
    }
  }

  pub fn start(&self) {
    let clone = self.clone();
    let clients_clone = self.clients.clone();

    std::thread::spawn(move || {
      loop {
        match clone.server.lock().unwrap().poll_event() {
          Event::Connect(client_id, responder) => {
            logger::log(format!("Client {} connected", client_id));

            // Send initial connection data
            responder.send(Message::Text(clone.data_on_connect.clone()));

            clients_clone.lock().unwrap().insert(client_id, responder);
          }
          Event::Disconnect(client_id) => {
            clients_clone.lock().unwrap().remove(&client_id);
          }
          Event::Message(client_id, message) => {
            logger::log(format!(
              "Received message from client {}: {:?}",
              client_id, message
            ));
            let responder = clients_clone.lock().unwrap();
            let responder = responder.get(&client_id).unwrap();
            responder.send(message);
          }
        }
      }
    });

    // Create a thread for each reciever
    let mut ipc_clone = self.clone();
    let mut proc_clone = self.clone();

    std::thread::spawn(move || {
      loop {
        let ipc_activity = ipc_clone.ipc_event_rec.lock().unwrap().recv().unwrap();

        // if there are no client, skip
        if ipc_clone.clients.lock().unwrap().len() == 0 {
          logger::log("No clients connected, skipping");
          continue;
        }

        if ipc_activity.args.activity.is_none() {
          // Send empty payload
          let payload = empty_activity(
            ipc_clone.last_pid.unwrap_or_default(),
            ipc_clone.last_socket_id.clone().unwrap_or_default(),
          );

          logger::log("Sending empty payload");

          ipc_clone.send_data(payload);

          continue;
        }

        let activity = ipc_activity.args.activity.as_ref();
        let button_urls: Vec<String> = match activity {
          Some(a) => a.buttons.iter().map(|x| x.url.clone()).collect(),
          None => vec![],
        };
        let button_labels: Vec<String> = match activity {
          Some(a) => a.buttons.iter().map(|x| x.label.clone()).collect(),
          None => vec![],
        };

        let payload = format!(
          // I don't even know what half of these fields are for yet
          r#"
          {{
            "activity": {{
              "application_id": "{}",
              "timestamps": {{
                "start": {}
              }},
              "assets": {},
              "details": "{}",
              "state": "{}",
              "type": 0,
              "buttons": {},
              "metadata": {{
                "button_urls": {}
              }},
              "flags": 0
            }},
            "pid": {},
            "socketId": "0"
          }}
          "#,
          ipc_activity.application_id.unwrap_or("".to_string()),
          ipc_activity
            .args
            .activity
            .as_ref()
            .unwrap()
            .timestamps
            .start,
          match activity {
            Some(a) => serde_json::to_string(&a.assets).unwrap(),
            None => "{}".to_string(),
          },
          match activity {
            Some(a) => a.details.clone(),
            None => "".to_string(),
          },
          match activity {
            Some(a) => a.state.clone(),
            None => "".to_string(),
          },
          serde_json::to_string(&button_labels).unwrap(),
          serde_json::to_string(&button_urls).unwrap(),
          ipc_activity.args.pid,
        );

        logger::log(&payload);

        logger::log("Sending payload for IPC activity");

        ipc_clone.send_data(payload);
      }
    });

    std::thread::spawn(move || {
      loop {
        let proc_event = proc_clone.proc_event_rec.lock().unwrap().recv().unwrap();
        let proc_activity = proc_event.activity;

        // if there are no client, skip
        if proc_clone.clients.lock().unwrap().len() == 0 {
          logger::log("No clients connected, skipping");
          continue;
        }

        if proc_activity.id == "null" {
          // Send empty payload
          let payload = empty_activity(
            proc_clone.last_pid.unwrap_or_default(),
            proc_clone.last_socket_id.clone().unwrap_or_default(),
          );

          logger::log("Sending empty payload");

          proc_clone.send_data(payload);

          continue;
        }

        let payload = format!(
          // I don't even know what half of these fields are for yet
          r#"
          {{
            "activity": {{
              "application_id": "{}",
              "name": "{}",
              "timestamps": {{
                "start": {}
              }},
              "type": 0,
              "metadata": {{}},
              "flags": 0
            }},
            "pid": {},
            "socketId": "{}"
          }}
          "#,
          proc_activity.id,
          proc_activity.name,
          proc_activity.timestamp.as_ref().unwrap(),
          proc_activity.pid.unwrap_or_default(),
          proc_activity.id
        );

        proc_clone.last_pid = proc_activity.pid;
        proc_clone.last_socket_id = Some(proc_activity.id.clone());

        logger::log(format!(
          "Sending payload for activity: {}",
          proc_activity.name
        ));

        proc_clone.send_data(payload);
      }
    });
  }

  pub fn send_data(&mut self, data: String) {
    // Send data to all clients
    for (_, responder) in self.clients.lock().unwrap().iter() {
      responder.send(Message::Text(data.clone()));
    }
  }
}

impl Drop for ClientConnector {
  fn drop(&mut self) {
    drop(self.server.lock().unwrap());
  }
}
