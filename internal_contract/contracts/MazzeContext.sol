pragma solidity >=0.4.15;

contract MazzeContext {
    /*** Query Functions ***/
    /**
     * @dev get the current epoch number
     * @return the current epoch number
     */
    function epochNumber() public view returns (uint256) {}
    function epochHash(uint256) external view returns (bytes32) {}
}
