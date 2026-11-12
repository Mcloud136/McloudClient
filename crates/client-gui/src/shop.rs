//! A3 商店状态与二维码渲染（内存编码，不落盘；二维码只含付款地址，不含凭据）。

use windows_reactor::EncodedImage;

use crate::panel::{CheckoutView, OrderView, PaymentMethodView, PlanView};

/// 进行中的订单会话。
#[derive(Clone)]
pub struct OrderSession {
    pub trade_no: String,
    pub status: i64,
    pub checkout: Option<CheckoutView>,
    pub qr: Option<EncodedImage>,
    pub started: std::time::Instant,
    pub last_poll: std::time::Instant,
}

/// 商店区状态（全部脱敏 DTO）。
#[derive(Clone, Default)]
pub struct ShopState {
    pub plans: Vec<PlanView>,
    pub methods: Vec<PaymentMethodView>,
    pub orders: Vec<OrderView>,
    pub plan_idx: usize,
    pub period_idx: usize,
    pub method_idx: usize,
    pub coupon_code: String,
    pub coupon_label: Option<String>,
    pub order: Option<OrderSession>,
    pub notice: Option<String>,
    pub loaded: bool,
}

impl ShopState {
    /// 当前选中套餐。
    #[must_use]
    pub fn plan(&self) -> Option<&PlanView> {
        self.plans.get(self.plan_idx)
    }

    /// 当前选中周期 (key, label, cents)。
    #[must_use]
    pub fn period(&self) -> Option<&(String, String, i64)> {
        self.plan().and_then(|p| p.prices.get(self.period_idx))
    }

    /// 当前选中支付方式 id。
    #[must_use]
    pub fn method_id(&self) -> Option<i64> {
        self.methods.get(self.method_idx).map(|m| m.id)
    }
}

/// 付款地址 → PNG 二维码（8 px/模块 + 4 模块静区）。
///
/// # Errors
///
/// 数据过长或 PNG 编码失败时返回错误。
pub fn qr_png(data: &str) -> anyhow::Result<Vec<u8>> {
    let code = qrcode::QrCode::new(data.as_bytes())?;
    let colors = code.to_colors();
    let width = code.width();
    let scale = 8usize;
    let quiet = 4usize;
    let side = (width + quiet * 2) * scale;
    let mut pixels = vec![255u8; side * side];
    for y in 0..width {
        for x in 0..width {
            if colors[y * width + x] == qrcode::Color::Dark {
                for dy in 0..scale {
                    for dx in 0..scale {
                        let px = (x + quiet) * scale + dx;
                        let py = (y + quiet) * scale + dy;
                        pixels[py * side + px] = 0;
                    }
                }
            }
        }
    }
    let mut buf = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut buf, side as u32, side as u32);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&pixels)?;
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    #[test]
    fn qr_png_produces_valid_png() {
        let bytes = super::qr_png("https://pay.example.invalid/qr/mock").expect("qr");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        assert!(bytes.len() > 100);
    }
}
