//! The AI host protocol (hub side: bridge/ai-nodes.js and vr-hub; the JavaScript host: bridge/ai-node.js): /node/hello,
//! /node/poll with its heartbeat, jobs, /node/reply, /node/stream, /node/events, /node/update. pgm-net-internal's
//! host link (PGM_Net_Internal) since 5.0.0, byte for byte the same.
pub use pgm_net_internal::hostlink::{Beat, EventsAnswer, Hello, HelloAnswer, HubLink, Job, ManifestFile, OfferedProvider, Poll, PollAnswer, Release, CAPS};
