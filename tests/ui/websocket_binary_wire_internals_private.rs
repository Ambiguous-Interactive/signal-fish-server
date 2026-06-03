use signal_fish_server::websocket::{
    encode_binary_game_data, sending, BinaryGameDataFrame,
};

fn main() {
    let _ = encode_binary_game_data;
    let _ = std::mem::size_of::<BinaryGameDataFrame<'static>>();
    let _ = std::any::type_name::<sending::ConnectionSender>();
}
