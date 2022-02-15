use systems::shared::{PositionPickoffUnit, LgciuSensors, AirDataSource, SfccChannel, FlapsConf, HandlePositionMemory, ElectricalBusType, ElectricalBuses};
use systems::navigation::adirs;
use systems::simulation::{
    InitContext, Read, SimulationElement, SimulationElementVisitor, SimulatorReader,
    SimulatorWriter, UpdateContext, VariableIdentifier, Write,
};

use crate::systems::shared::arinc429::{Arinc429Word, SignStatus};

use std::panic;
use uom::si::{angle::degree, f64::*, velocity::knot};



fn demanded_flaps_angle_from_conf(flap_conf: FlapsConf) -> Angle {
    match flap_conf {
        FlapsConf::Conf0 => Angle::new::<degree>(0.),
        FlapsConf::Conf1 => Angle::new::<degree>(0.),
        FlapsConf::Conf1F => Angle::new::<degree>(10.),
        FlapsConf::Conf2 => Angle::new::<degree>(15.),
        FlapsConf::Conf3 => Angle::new::<degree>(20.),
        FlapsConf::ConfFull => Angle::new::<degree>(40.),
    }
}

fn demanded_slats_angle_from_conf(flap_conf: FlapsConf) -> Angle {
    match flap_conf {
        FlapsConf::Conf0 => Angle::new::<degree>(0.),
        FlapsConf::Conf1 => Angle::new::<degree>(18.),
        FlapsConf::Conf1F => Angle::new::<degree>(18.),
        FlapsConf::Conf2 => Angle::new::<degree>(22.),
        FlapsConf::Conf3 => Angle::new::<degree>(22.),
        FlapsConf::ConfFull => Angle::new::<degree>(27.),
    }
}

struct FlapsHandle {
    handle_position_id: VariableIdentifier,
    position: u8,
    previous_position: u8,
}

impl FlapsHandle {
    fn new(context: &mut InitContext) -> Self {
        Self {
            handle_position_id: context.get_identifier("FLAPS_HANDLE_INDEX".to_owned()),
            position: 0,
            previous_position: 0,
        }
    }
}
impl HandlePositionMemory for FlapsHandle {
    fn position(&self) -> u8 {
        self.position
    }

    fn previous_position(&self) -> u8 {
        self.previous_position
    }
}

impl SimulationElement for FlapsHandle {
    fn read(&mut self, reader: &mut SimulatorReader) {
        self.previous_position = self.position;
        self.position = reader.read(&self.handle_position_id);
    }
}


struct FlapsChannel {
    is_powered: bool,
    powered_by: ElectricalBusType,

    feedback_angle: Angle,
    demanded_angle: Angle,
    calculated_conf: FlapsConf,

    sfcc_id: usize,
}

impl FlapsChannel {
    const HANDLE_ONE_CONF_AIRSPEED_THRESHOLD_KNOTS: f64 = 100.;
    const CONF1F_TO_CONF1_AIRSPEED_THRESHOLD_KNOTS: f64 = 210.;
    const EQUAL_ANGLE_DELTA_DEGREE: f64 = 0.01;

    fn new(context: &mut InitContext, powered_by: ElectricalBusType, sfcc_id: usize) -> Self {
        Self {
            is_powered: false,
            powered_by,

            feedback_angle: Angle::new::<degree>(0.),
            demanded_angle: Angle::new::<degree>(0.),
            calculated_conf: FlapsConf::Conf0,

            sfcc_id,
        }
    }

    pub fn update(&mut self, context: &UpdateContext, flaps_handle: &impl HandlePositionMemory, feedback: &impl PositionPickoffUnit, adiru: &impl AirDataSource) {

        self.receive_signal_fppu(feedback);
        self.calculated_conf = self.generate_configuration(context,flaps_handle,adiru);
        self.demanded_angle = demanded_flaps_angle_from_conf(self.calculated_conf);
    }

    fn feedback_equals_demanded(&self) -> bool {
        (self.demanded_angle - self.feedback_angle).get::<degree>().abs() < Self::EQUAL_ANGLE_DELTA_DEGREE
    }
}

impl SfccChannel for FlapsChannel {


    fn receive_signal_fppu(&mut self, feedback: &impl PositionPickoffUnit) {
        self.feedback_angle = feedback.angle();
    }

    fn send_signal_to_motors(&self) -> bool {
        !self.feedback_equals_demanded()
    }

    fn generate_configuration(
        &self,
        context: &UpdateContext,
        flaps_handle: &impl HandlePositionMemory,
        adiru: &impl AirDataSource,
    ) -> FlapsConf {

        let computed_airspeed: Velocity =
            match adiru.computed_airspeed().ssm() {
                SignStatus::NormalOperation => adiru.computed_airspeed().value(),
                _ => context.indicated_airspeed(),
            };

        match (flaps_handle.previous_position(), flaps_handle.position()) {
            (0, 1)
                if computed_airspeed.get::<knot>()
                    <= Self::HANDLE_ONE_CONF_AIRSPEED_THRESHOLD_KNOTS =>
            {
                FlapsConf::Conf1F
            }
            (0, 1) => FlapsConf::Conf1,
            (1, 1)
                if computed_airspeed.get::<knot>()
                    > Self::CONF1F_TO_CONF1_AIRSPEED_THRESHOLD_KNOTS =>
            {
                FlapsConf::Conf1
            }
            (1, 1) => self.calculated_conf,
            (_, 1)
                if computed_airspeed.get::<knot>()
                    <= Self::CONF1F_TO_CONF1_AIRSPEED_THRESHOLD_KNOTS =>
            {
                FlapsConf::Conf1F
            }
            (_, 1) => FlapsConf::Conf1,
            (_, 0) => FlapsConf::Conf0,
            (from, to) if from != to => FlapsConf::from(to + 1),
            (_, _) => self.calculated_conf,
        }
    }
}

impl SimulationElement for FlapsChannel {
    fn receive_power(&mut self, buses: &impl ElectricalBuses) {
        self.is_powered = buses.is_powered(self.powered_by);
    }
}


struct SlatsChannel {

    is_powered: bool,
    powered_by: ElectricalBusType,


    feedback_angle: Angle,
    demanded_angle: Angle,
    calculated_conf: FlapsConf,

    is_on_ground: bool,

    alpha_lock_engaged: bool,
    alpha_lock_engaged_alpha: bool,
    alpha_lock_engaged_speed: bool,

    sfcc_id: usize,
}
impl SlatsChannel {

    const EQUAL_ANGLE_DELTA_DEGREE: f64 = 0.01;
    const ALPHA_LOCK_ENGAGE_SPEED_KNOTS: f64 = 148.;
    const ALPHA_LOCK_DISENGAGE_SPEED_KNOTS: f64 = 154.;
    const ALPHA_LOCK_ENGAGE_ALPHA_DEGREES: f64 = 8.6;
    const ALPHA_LOCK_DISENGAGE_ALPHA_DEGREES: f64 = 7.6;

    fn new(context: &mut InitContext, powered_by: ElectricalBusType, sfcc_id: usize) -> Self {
        Self {
            is_powered: false,
            powered_by,
            feedback_angle: Angle::new::<degree>(0.),
            demanded_angle: Angle::new::<degree>(0.),
            calculated_conf: FlapsConf::Conf0,
            is_on_ground: true,
            alpha_lock_engaged: false,
            alpha_lock_engaged_alpha: false,
            alpha_lock_engaged_speed: false,
            sfcc_id
        }
    }

    pub fn update(&mut self, context: &UpdateContext, flaps_handle: &impl HandlePositionMemory, feedback: &impl PositionPickoffUnit, adiru: &impl AirDataSource, is_on_ground: bool) {

        self.receive_signal_fppu(feedback);
        self.is_on_ground = is_on_ground;
        self.alpha_lock_check(context, flaps_handle, adiru);
        self.calculated_conf = self.generate_configuration(context, flaps_handle, adiru);
        self.demanded_angle = demanded_slats_angle_from_conf(self.calculated_conf);

    }

    pub fn update_is_on_ground(&mut self, is_on_ground: bool) {
        self.is_on_ground = is_on_ground;
    }

    fn feedback_equals_demanded(&self) -> bool {
        (self.demanded_angle - self.feedback_angle).get::<degree>().abs() < Self::EQUAL_ANGLE_DELTA_DEGREE
    }

    fn alpha_lock_check(&mut self, context: &UpdateContext, flaps_handle: &impl HandlePositionMemory, adiru: &impl AirDataSource) {
        let airspeed: Velocity =
            match adiru.computed_airspeed().ssm() {
                SignStatus::NormalOperation => adiru.computed_airspeed().value(),
                _ => context.indicated_airspeed(),
            };
        let alpha: Angle =  match adiru.alpha().ssm() {
            SignStatus::NormalOperation => adiru.alpha().value(),
            _ => context.alpha(),
        };


        match self.alpha_lock_engaged_speed {
            true => if airspeed.get::<knot>() > Self::ALPHA_LOCK_DISENGAGE_SPEED_KNOTS {
                self.alpha_lock_engaged_speed = false;
            }
            false => if airspeed.get::<knot>() < Self::ALPHA_LOCK_ENGAGE_SPEED_KNOTS {
                self.alpha_lock_engaged_speed = true;
            }
        }

        match self.alpha_lock_engaged_alpha {
            true => if alpha.get::<degree>() < Self::ALPHA_LOCK_DISENGAGE_ALPHA_DEGREES {
                self.alpha_lock_engaged_alpha = false;
            }
            false => if alpha.get::<degree>() > Self::ALPHA_LOCK_ENGAGE_ALPHA_DEGREES {
                self.alpha_lock_engaged_alpha = true;
            }
        }

        match self.alpha_lock_engaged {
            false =>
                if (self.alpha_lock_engaged_alpha || self.alpha_lock_engaged_speed)
                    && flaps_handle.position() > 0
                    && !(self.is_on_ground && airspeed.get::<knot>() < 60.)
                    {
                        self.alpha_lock_engaged = true;
                    }

            true => self.alpha_lock_engaged = self.alpha_lock_engaged_speed || self.alpha_lock_engaged_alpha
        }
    }
}

impl SfccChannel for SlatsChannel {


    fn receive_signal_fppu(&mut self, feedback: &impl PositionPickoffUnit) {
        self.feedback_angle = feedback.angle();
    }

    fn send_signal_to_motors(&self) -> bool {
        !self.feedback_equals_demanded()
    }


    fn generate_configuration(
        &self,
        _context: &UpdateContext,
        flaps_handle: &impl HandlePositionMemory,
        adiru: &impl AirDataSource,
    ) -> FlapsConf {
        match (flaps_handle.previous_position(), flaps_handle.position()) {
            (_, to) if to != 0 => FlapsConf::from(to),
            (from, 0) if self.alpha_lock_engaged && from > 0 => FlapsConf::Conf1,
            (_, 0) => FlapsConf::Conf0,
            (_, _) => self.calculated_conf,
        }
    }
}

impl SimulationElement for SlatsChannel {
    fn receive_power(&mut self, buses: &impl ElectricalBuses) {
        self.is_powered = buses.is_powered(self.powered_by);
    }
}


struct SlatsFlapsControlComputer {
    flaps_channel: FlapsChannel,
    slats_channel: SlatsChannel,
    is_on_ground: bool,

    sfcc_id: usize,

    flaps_demanded_angle_id: VariableIdentifier,
    slats_demanded_angle_id: VariableIdentifier,
    alpha_lock_engaged_id: VariableIdentifier,
}

impl SlatsFlapsControlComputer {
    pub fn new(context: &mut InitContext, sfcc_id: usize, flaps_channel_bus: ElectricalBusType, slats_channel_bus: ElectricalBusType) -> Self {
        Self {
            flaps_channel: FlapsChannel::new(context, flaps_channel_bus, sfcc_id),
            slats_channel: SlatsChannel::new(context, slats_channel_bus, sfcc_id),
            is_on_ground: true,
            sfcc_id,
            flaps_demanded_angle_id: context.get_identifier(format!("SFCC_{}_FLAPS_DEMANDED_ANGLE", sfcc_id)),
            slats_demanded_angle_id: context.get_identifier(format!("SFCC_{}_SLATS_DEMANDED_ANGLE", sfcc_id)),
            alpha_lock_engaged_id: context.get_identifier(format!("SFCC_{}_ALPHA_LOCK_ENGAGED", sfcc_id)),
        }
    }

    pub fn update(&mut self, context: &UpdateContext, flaps_handle: &impl HandlePositionMemory,
        adirus: [&impl AirDataSource; 2], lgciu: &impl LgciuSensors,
        flaps_fppu: &impl PositionPickoffUnit, slats_fppu: &impl PositionPickoffUnit) {

        self.is_on_ground = lgciu.left_and_right_gear_compressed(false);
        self.flaps_channel.update(context, flaps_handle, flaps_fppu, adirus[0]);
        self.slats_channel.update(context, flaps_handle, slats_fppu, adirus[1], self.is_on_ground);
    }
}

impl SimulationElement for SlatsFlapsControlComputer {
    fn accept<T: SimulationElementVisitor>(&mut self, visitor: &mut T) {
        self.flaps_channel.accept(visitor);
        self.slats_channel.accept(visitor);
        visitor.visit(self);
    }

    fn write(&self, writer: &mut SimulatorWriter) {
        writer.write(&self.flaps_demanded_angle_id, self.flaps_channel.demanded_angle);
        writer.write(&self.slats_demanded_angle_id, self.slats_channel.demanded_angle);
        writer.write(&self.alpha_lock_engaged_id, self.slats_channel.alpha_lock_engaged);
    }
}

struct SlatsFlapsElectronicComplex {
    sfcc: [SlatsFlapsControlComputer; 2],
    flaps_handle: FlapsHandle,
}


//SFCC1 channels supplied by sub bus 401pp
//SFCC2 flap channel supplied by 204pp and slats channel by 202pp
impl SlatsFlapsElectronicComplex {
    pub fn new(context: &mut InitContext) -> Self {
        Self {
            sfcc: [SlatsFlapsControlComputer::new(context,1, ElectricalBusType::DirectCurrentEssential, ElectricalBusType::DirectCurrentEssential),
                 SlatsFlapsControlComputer::new(context,2, ElectricalBusType::DirectCurrent(2), ElectricalBusType::DirectCurrent(2))],
            flaps_handle: FlapsHandle::new(context),
        }
    }

    pub fn update(&mut self, context: &UpdateContext, adirus: [&impl AirDataSource; 2], lgcius: [&impl LgciuSensors; 2],
        flaps_fppu: &impl PositionPickoffUnit, slats_fppu: &impl PositionPickoffUnit) {
            self.sfcc[0].update(context, &self.flaps_handle, adirus, lgcius[0], flaps_fppu, slats_fppu);
            self.sfcc[1].update(context, &self.flaps_handle, adirus, lgcius[1], flaps_fppu, slats_fppu);
    }

}

impl SimulationElement for SlatsFlapsElectronicComplex {
    fn accept<T: SimulationElementVisitor>(&mut self, visitor: &mut T) {
        self.flaps_handle.accept(visitor);
        self.sfcc[0].accept(visitor);
        self.sfcc[1].accept(visitor);
        visitor.visit(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntest::assert_about_eq;
    use std::time::Duration;
    use systems::simulation::{
        test::{ReadByName, SimulationTestBed, TestBed, WriteByName},
        Aircraft,
    };
    use systems::landing_gear::{LandingGearControlInterfaceUnit};
    use systems::navigation::adirs::{AirDataInertialReferenceSystem};
    use systems::shared::{LgciuWeightOnWheels,LgciuSensors};
    use uom::si::{angular_velocity::degree_per_second, pressure::psi};

    struct SfccTestFppu {
        feedback_angle: Angle,
    }

    impl SfccTestFppu {
        fn new() -> Self {
            Self {
                feedback_angle: Angle::new::<degree>(0.),
            }
        }
    }

    impl PositionPickoffUnit for SfccTestFppu {
        fn angle(&self) -> Angle {
            self.feedback_angle
        }
    }

    struct A320SfccTestAircraft {
        sf_electronic_complex: SlatsFlapsElectronicComplex,
        lgcius: [LandingGearControlInterfaceUnit; 2],
        adirs: AirDataInertialReferenceSystem,
        slats_fppu: SfccTestFppu,
        flaps_fppu: SfccTestFppu,
    }

    impl A320SfccTestAircraft {
        fn new(context: &mut InitContext) -> Self {
            Self {
                sf_electronic_complex: SlatsFlapsElectronicComplex::new(context),
                lgcius: [LandingGearControlInterfaceUnit::new(
                    ElectricalBusType::DirectCurrentEssential),LandingGearControlInterfaceUnit::new(ElectricalBusType::DirectCurrent(2))],
                adirs: AirDataInertialReferenceSystem::new(context),
                slats_fppu: SfccTestFppu::new(),
                flaps_fppu: SfccTestFppu::new(),
            }
        }
    }

    impl Aircraft for A320SfccTestAircraft {
        fn update_after_power_distribution(&mut self, context: &UpdateContext) {
            self.sf_electronic_complex.update(context, [self.adirs.adirus(0), self.adirs.adirus(1)],[&self.lgcius[0], &self.lgcius[1]],
                        &self.flaps_fppu, &self.slats_fppu);
        }
    }
    impl SimulationElement for A320SfccTestAircraft {
        fn accept<T: SimulationElementVisitor>(&mut self, visitor: &mut T) {
            self.sf_electronic_complex.accept(visitor);
            visitor.visit(self);
        }

        // fn read(&mut self, reader: &mut SimulatorReader) {

        // }
    }

    struct A320FlapsTestBed {
        test_bed: SimulationTestBed<A320SfccTestAircraft>,
    }

    impl A320FlapsTestBed {
        const HYD_TIME_STEP_MILLIS: u64 = 33;

        fn new() -> Self {
            Self {
                test_bed: SimulationTestBed::new(A320SfccTestAircraft::new),
            }
        }

        fn run_one_tick(mut self) -> Self {
            self.test_bed
                .run_with_delta(Duration::from_millis(Self::HYD_TIME_STEP_MILLIS));
            self
        }

        fn run_waiting_for(mut self, delta: Duration) -> Self {
            self.test_bed.run_multiple_frames(delta);
            self
        }

        fn set_flaps_handle_position(mut self, pos: u8) -> Self {
            self.write_by_name("FLAPS_HANDLE_INDEX", pos as f64);
            self
        }

        fn read_flaps_handle_position(&mut self) -> u8 {
            self.read_by_name("FLAPS_HANDLE_INDEX")
        }

        fn set_indicated_airspeed(mut self, indicated_airspeed: f64) -> Self {
            self.write_by_name("AIRSPEED INDICATED", indicated_airspeed);
            self
        }

        fn get_flaps_demanded_angle(&self,sfcc_id: usize) -> f64 {
            self.query(|a| {
                a.sf_electronic_complex.sfcc[sfcc_id]
                .flaps_channel.demanded_angle.get::<degree>()
                }
            )
        }
        fn get_slats_demanded_angle(&self,sfcc_id: usize) -> f64 {
            self.query(|a| {
                a.sf_electronic_complex.sfcc[sfcc_id]
                .slats_channel.demanded_angle.get::<degree>()
                }
            )
        }

        fn get_flaps_conf(&self, sfcc_id: usize) -> FlapsConf {
            self.query(|a| {
                a.sf_electronic_complex.sfcc[sfcc_id]
                    .flaps_channel.calculated_conf
            })
        }

        fn get_slats_conf(&self, sfcc_id: usize) -> FlapsConf {
            self.query(|a| {
                a.sf_electronic_complex.sfcc[sfcc_id]
                    .slats_channel.calculated_conf
            })
        }

        fn test_conf(
            &mut self,
            handle_pos: u8,
            flaps_demanded_angle: f64,
            slats_demanded_angle: f64,
            slats_conf: FlapsConf,
            flaps_conf: FlapsConf,
            angle_delta: f64,
            sfcc_id: usize,
        ) {
            assert_eq!(self.read_flaps_handle_position(), handle_pos);
            assert!((self.get_flaps_demanded_angle(sfcc_id) - flaps_demanded_angle).abs() < angle_delta);
            assert!((self.get_slats_demanded_angle(sfcc_id) - slats_demanded_angle).abs() < angle_delta);
            assert_eq!(self.get_flaps_conf(sfcc_id), flaps_conf);
            assert_eq!(self.get_slats_conf(sfcc_id), slats_conf);
        }

        fn lgciu_on_ground(&self) -> bool {
            self.query(|a| {
                a.lgcius[0].left_and_right_gear_compressed(false)
            })
        }
    }
    impl TestBed for A320FlapsTestBed {
        type Aircraft = A320SfccTestAircraft;

        fn test_bed(&self) -> &SimulationTestBed<A320SfccTestAircraft> {
            &self.test_bed
        }

        fn test_bed_mut(&mut self) -> &mut SimulationTestBed<A320SfccTestAircraft> {
            &mut self.test_bed
        }
    }

    fn test_bed() -> A320FlapsTestBed {
        A320FlapsTestBed::new()
    }

    fn test_bed_with(on_ground: bool) -> A320FlapsTestBed {
        let mut test_bed = test_bed();
        test_bed.set_on_ground(on_ground);
        test_bed
    }

    #[test]
    fn dummy_test() {
        assert!(1==1)
    }

    #[test]
    fn test_lgciu_on_ground() {
        let mut test_bed = test_bed_with(true);

        assert!(test_bed.lgciu_on_ground());
        test_bed = test_bed_with(true);
        assert!(!test_bed.lgciu_on_ground())
    }
}
